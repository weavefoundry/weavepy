//! The `binascii` built-in module.
//!
//! The CPython `binascii` API is a grab-bag of byte-level utilities
//! that grew up around uuencode / yEnc / quoted-printable / CRC-32.
//! We ship the modern-day subset everyday programs actually use:
//!
//! * `b2a_hex` / `hexlify` / `a2b_hex` / `unhexlify` (with the
//!   `sep`/`bytes_per_sep` grouping arguments)
//! * `b2a_base64` / `a2b_base64` (the keyword-only `newline=` /
//!   `strict_mode=` flags the verbatim `base64.py` port relies on, with
//!   the exact CPython padding/validation error messages)
//! * `b2a_uu` / `a2b_uu` (uuencode; line-length byte, `backtick=` zero
//!   padding, "Illegal char"/"Trailing garbage" diagnostics)
//! * `b2a_qp` / `a2b_qp` (quoted-printable; the 76-column soft-wrap,
//!   CRLF detection/normalisation, and `quotetabs=`/`istext=`/`header=`
//!   knobs, ported byte-for-byte from `Modules/binascii.c`)
//! * `crc32` / `crc_hqx`
//! * `Error` / `Incomplete`

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
            Object::from_static("binascii"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Conversions between binary data and various ASCII encodings."),
        );
        d.insert(
            DictKey(Object::from_static("Error")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("Incomplete")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        // Plain positional-only helpers.
        for (name, body) in [
            (
                "a2b_hex",
                a2b_hex as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("unhexlify", a2b_hex),
            ("a2b_uu", a2b_uu),
            ("crc32", crc32),
            ("crc_hqx", crc_hqx),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
        // Keyword-aware helpers. `hexlify(data, sep, bytes_per_sep)`,
        // `b2a_base64(data, *, newline=True)`,
        // `a2b_base64(data, *, strict_mode=False)`, the uuencode/
        // quoted-printable pair, etc.
        register_kw(&mut d, "b2a_hex", b2a_hex_kw);
        register_kw(&mut d, "hexlify", b2a_hex_kw);
        register_kw(&mut d, "b2a_base64", b2a_base64_kw);
        register_kw(&mut d, "a2b_base64", a2b_base64_kw);
        register_kw(&mut d, "b2a_uu", b2a_uu_kw);
        register_kw(&mut d, "a2b_qp", a2b_qp_kw);
        register_kw(&mut d, "b2a_qp", b2a_qp_kw);
    }
    Rc::new(PyModule {
        name: "binascii".to_owned(),
        filename: None,
        dict,
    })
}

fn register_kw(
    d: &mut DictData,
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) {
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    );
}

/// Strict bytes-like extraction for the *encode* (`b2a_*`, `crc*`) side:
/// bytes / bytearray / memoryview / any buffer-protocol object, but **not**
/// `str`. CPython's `b2a_base64`/`hexlify` reject `str` (test_base64's
/// `check_encode_type_errors` asserts `b64encode("")` raises `TypeError`).
fn buffer_bytes(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    let obj = match arg {
        Some(o) => o,
        None => {
            return Err(type_error(
                "a bytes-like object is required, not 'NoneType'",
            ))
        }
    };
    // CPython requests a C-contiguous buffer (`PyArg_Parse` `y*`); a strided
    // view such as `memoryview(bytearray(b'...'))[::-2]` raises `BufferError`
    // rather than being silently gathered.
    if let Object::MemoryView(mv) = obj {
        if !mv.is_c_contiguous() {
            return Err(crate::error::buffer_error(
                "memoryview: underlying buffer is not C-contiguous",
            ));
        }
    }
    if let Some(v) = obj.as_bytes_view() {
        return Ok(v);
    }
    if let Object::Instance(_) = obj {
        // A subclass of `bytes`/`bytearray` exposes the inherited buffer
        // through its native payload (CPython's `PyArg_ParseTuple` `t#`);
        // a non-buffer object that nonetheless implements the buffer
        // protocol in Python (`array.array`, custom readers) is read via
        // its `tobytes()` — the WeavePy stand-in for `bf_getbuffer`. This
        // lets `test_base64`/`test_binascii` feed `array('B', …)` and
        // `memoryview(...)` into every codec.
        if let Some(native) = obj.native_value() {
            if let Some(v) = native.as_bytes_view() {
                return Ok(v);
            }
        }
        if let Some(v) = buffer_via_tobytes(obj)? {
            return Ok(v);
        }
    }
    Err(type_error(format!(
        "a bytes-like object is required, not '{}'",
        obj.type_name()
    )))
}

/// Bytes-like extraction for the *decode* (`a2b_*`) side, which also accepts
/// an ASCII `str` (CPython coerces it via the codec's `s#`/`y*` parsing).
fn input_bytes(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    if let Some(Object::Str(s)) = arg {
        if !s.is_ascii() {
            return Err(crate::error::value_error(
                "string argument should contain only ASCII characters",
            ));
        }
        return Ok(s.as_bytes().to_vec());
    }
    buffer_bytes(arg)
}

/// Buffer-protocol fallback: if `obj` has a `tobytes()` method (e.g.
/// `array.array`), call it through interpreter reentry — the same pattern
/// `coerce_index_i64` uses for `__index__`. Returns `Ok(None)` when there
/// is no such method so the caller can raise its own `TypeError`.
fn buffer_via_tobytes(obj: &Object) -> Result<Option<Vec<u8>>, RuntimeError> {
    let Some(method) = crate::instance_method(obj, "tobytes") else {
        return Ok(None);
    };
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(None);
    };
    // SAFETY: the pointer was published by an enclosing VM frame still live
    // on this thread; the GIL keeps the access exclusive (mirrors
    // `coerce_index_i64`).
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    let result = interp.call_object_with_globals(&method, &[], &[], &globals)?;
    Ok(result.as_bytes_view())
}

fn truthy_kw(kwargs: &[(String, Object)], key: &str) -> Option<bool> {
    kwargs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.is_truthy())
}

// ---- hex ----

fn b2a_hex_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = buffer_bytes(args.first())?;
    // sep / bytes_per_sep, positional or keyword. CPython:
    // `b2a_hex(data[, sep[, bytes_per_sep=1]])`.
    let sep_obj = args.get(1).cloned().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == "sep")
            .map(|(_, v)| v.clone())
    });
    let bps = args
        .get(2)
        .cloned()
        .or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == "bytes_per_sep")
                .map(|(_, v)| v.clone())
        })
        .and_then(|o| o.as_i64())
        .unwrap_or(1);

    let hex: Vec<u8> = {
        let mut out = Vec::with_capacity(data.len() * 2);
        for &b in &data {
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
        out
    };

    let sep = match sep_obj {
        None | Some(Object::None) => None,
        Some(o) => {
            let s = match &o {
                Object::Str(s) if s.len() == 1 && s.is_ascii() => s.as_bytes()[0],
                Object::Bytes(b) if b.len() == 1 => b[0],
                Object::ByteArray(b) if b.borrow().len() == 1 => b.borrow()[0],
                Object::Str(_) => return Err(value_error("sep must be ASCII.")),
                _ => return Err(type_error("sep must be str or bytes.")),
            };
            Some(s)
        }
    };

    let Some(sep) = sep else {
        return Ok(Object::new_bytes(hex));
    };
    if bps == 0 {
        return Err(value_error("bytes_per_sep must not be zero"));
    }
    Ok(Object::new_bytes(insert_sep(&data, &hex, sep, bps)))
}

/// Insert `sep` every `bytes_per_sep` *input* bytes (each input byte is
/// two hex chars). Positive groups from the right, negative from the left
/// — mirrors CPython's `binascii.hexlify`.
fn insert_sep(data: &[u8], hex: &[u8], sep: u8, bytes_per_sep: i64) -> Vec<u8> {
    let n = data.len();
    let group = bytes_per_sep.unsigned_abs() as usize;
    if group >= n {
        return hex.to_vec();
    }
    let mut out = Vec::with_capacity(hex.len() + n / group);
    if bytes_per_sep < 0 {
        // Group from the left: sep after each `group` bytes.
        for (i, byte_idx) in (0..n).enumerate() {
            if i != 0 && i.is_multiple_of(group) {
                out.push(sep);
            }
            out.push(hex[byte_idx * 2]);
            out.push(hex[byte_idx * 2 + 1]);
        }
    } else {
        // Group from the right: sep before each `group`-byte block,
        // counting from the end.
        for byte_idx in 0..n {
            if byte_idx != 0 && (n - byte_idx).is_multiple_of(group) {
                out.push(sep);
            }
            out.push(hex[byte_idx * 2]);
            out.push(hex[byte_idx * 2 + 1]);
        }
    }
    out
}

fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }
}

fn a2b_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    if !data.len().is_multiple_of(2) {
        return Err(value_error("Odd-length string"));
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(Object::new_bytes(out))
}

fn hex_value(c: u8) -> Result<u8, RuntimeError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(value_error("Non-hexadecimal digit found")),
    }
}

// ---- base64 ----

/// Standard base64 alphabet decode table: maps a byte to its 6-bit value,
/// or -1 if it is not a base64 digit. `=` (pad) is handled separately.
fn b64_decode_value(c: u8) -> i16 {
    match c {
        b'A'..=b'Z' => i16::from(c - b'A'),
        b'a'..=b'z' => i16::from(c - b'a') + 26,
        b'0'..=b'9' => i16::from(c - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => -1,
    }
}

const B64_ENCODE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b2a_base64_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = buffer_bytes(args.first())?;
    // CPython: `b2a_base64(data, /, *, newline=True)`.
    let newline = truthy_kw(kwargs, "newline").unwrap_or(true);

    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4 + 1);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(B64_ENCODE[(b0 >> 2) as usize]);
        out.push(B64_ENCODE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize]);
        if chunk.len() > 1 {
            out.push(B64_ENCODE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize]);
        } else {
            out.push(b'=');
        }
        if chunk.len() > 2 {
            out.push(B64_ENCODE[(b2 & 0x3f) as usize]);
        } else {
            out.push(b'=');
        }
    }
    if newline {
        out.push(b'\n');
    }
    Ok(Object::new_bytes(out))
}

/// Faithful port of CPython's `binascii_a2b_base64_impl` (Modules/binascii.c):
/// the quad/pad state machine plus the exact strict-mode error messages that
/// `test_binascii` and `base64.b64decode(validate=True)` assert on.
fn a2b_base64_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let strict = truthy_kw(kwargs, "strict_mode").unwrap_or(false);

    let mut out: Vec<u8> = Vec::with_capacity(data.len().div_ceil(4) * 3);
    let mut quad_pos: i32 = 0;
    let mut leftchar: u8 = 0;
    let mut pads: i32 = 0;

    for (i, &this_ch) in data.iter().enumerate() {
        if this_ch == b'=' {
            // Pad handling (RFC 4648 §3.3). A *valid* pad — one that closes
            // a quad that already holds ≥ 2 data chars — is consumed and we
            // keep going; lenient mode also silently swallows leading/excess
            // pads. Only strict mode rejects malformed padding.
            pads += 1;
            if quad_pos >= 2 && quad_pos + pads <= 4 {
                continue;
            }
            if !strict {
                continue;
            }
            if quad_pos == 1 {
                // Falls through to the length error below.
                break;
            }
            return Err(value_error(if quad_pos == 0 && i == 0 {
                "Leading padding not allowed"
            } else {
                "Excess padding not allowed"
            }));
        }

        let val = b64_decode_value(this_ch);
        if val < 0 {
            if strict {
                return Err(value_error("Only base64 data is allowed"));
            }
            continue;
        }
        // A real data char following pad chars: in strict mode this is
        // either trailing data after a complete quad, or discontinuous
        // padding. Lenient mode just resumes decoding.
        if pads != 0 && strict {
            return Err(value_error(if quad_pos + pads == 4 {
                "Excess data after padding"
            } else {
                "Discontinuous padding not allowed"
            }));
        }
        pads = 0;
        let val = val as u8;
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = val;
            }
            1 => {
                quad_pos = 2;
                out.push((leftchar << 2) | (val >> 4));
                leftchar = val & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((leftchar << 4) | (val >> 2));
                leftchar = val & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((leftchar << 6) | val);
                leftchar = 0;
            }
        }
    }

    if quad_pos == 1 {
        // Exactly one extra non-pad char: no byte string encodes to this.
        let ndata = out.len() as i64 / 3 * 4 + 1;
        return Err(value_error(format!(
            "Invalid base64-encoded string: number of data characters ({ndata}) cannot be 1 more than a multiple of 4"
        )));
    }
    if quad_pos != 0 && quad_pos + pads < 4 {
        return Err(value_error("Incorrect padding"));
    }
    Ok(Object::new_bytes(out))
}

// ---- CRC ----

fn crc32(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = buffer_bytes(args.first())?;
    let init = match args.get(1) {
        Some(Object::Int(n)) => *n as u32,
        None | Some(Object::None) => 0,
        _ => return Err(type_error("crc32: seed must be int")),
    };
    let mut hasher = crc32fast::Hasher::new_with_initial(init);
    hasher.update(&data);
    Ok(Object::Int(i64::from(hasher.finalize())))
}

fn crc_hqx(args: &[Object]) -> Result<Object, RuntimeError> {
    // Mac BinHex CRC-16/HQX. Used by a handful of legacy formats; we
    // implement the canonical polynomial for completeness. Both arguments
    // are required (`test_crc_hqx` asserts `crc_hqx(b'')` — one arg — is a
    // `TypeError`).
    if args.len() < 2 {
        return Err(type_error(format!(
            "crc_hqx() takes exactly 2 arguments ({} given)",
            args.len()
        )));
    }
    let data = buffer_bytes(args.first())?;
    let init = match args.get(1) {
        Some(Object::Int(n)) => *n as u16,
        Some(Object::None) => 0,
        _ => return Err(type_error("crc_hqx: seed must be int")),
    };
    let mut crc = init;
    for &b in &data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    Ok(Object::Int(i64::from(crc)))
}

// ---- uuencode (b2a_uu / a2b_uu) ----

/// Decode one uuencoded line. Faithful port of CPython's
/// `binascii_a2b_uu_impl`: the first byte carries the binary length
/// (`(c - ' ') & 0o77`); the remainder are 6-bit groups, with newlines and
/// short lines treated as zero padding. A data byte outside `[' ', '`']`
/// is an "Illegal char"; non-whitespace past the declared length is
/// "Trailing garbage".
fn a2b_uu(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    // An empty line decodes to empty output (the vendored `test_empty_string`
    // feeds `a2b_uu(b'')` and asserts it does not raise).
    if data.is_empty() {
        return Ok(Object::new_bytes(Vec::new()));
    }
    let bin_len = i64::from(data[0].wrapping_sub(b' ') & 0o77);
    let rest = &data[1..];

    let mut out = Vec::with_capacity(bin_len.max(0) as usize);
    let mut leftchar: u32 = 0;
    let mut leftbits: i32 = 0;
    let mut produced: i64 = 0;
    let mut i = 0usize;
    while produced < bin_len {
        let have = i < rest.len();
        let c = if have { rest[i] } else { 0 };
        // Whitespace / end-of-data shifts in zero bits (CPython assumes
        // trailing spaces were eaten by some mailer); anything below ' '
        // or above '`' is rejected outright.
        let this_ch: u8 = if !have || c == b'\n' || c == b'\r' {
            0
        } else if !(b' '..=b' ' + 64).contains(&c) {
            return Err(value_error("Illegal char"));
        } else {
            c.wrapping_sub(b' ') & 0o77
        };
        i += 1;
        leftchar = (leftchar << 6) | u32::from(this_ch);
        leftbits += 6;
        if leftbits >= 8 {
            leftbits -= 8;
            out.push(((leftchar >> leftbits) & 0xff) as u8);
            leftchar &= (1u32 << leftbits) - 1;
            produced += 1;
        }
    }
    // Whatever remains on the line must be padding/whitespace.
    while i < rest.len() {
        let c = rest[i];
        if c != b' ' && c != b' ' + 64 && c != b'\n' && c != b'\r' {
            return Err(value_error("Trailing garbage"));
        }
        i += 1;
    }
    Ok(Object::new_bytes(out))
}

/// Uuencode a chunk (≤ 45 bytes). Faithful port of
/// `binascii_b2a_uu_impl`: emit the length byte, pack 8-bit input into
/// 6-bit output groups, and append a courtesy newline. With `backtick=True`
/// a zero group is written as `` ` `` instead of a space.
fn b2a_uu_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_unknown_kwargs("b2a_uu", kwargs, &["backtick"])?;
    // `backtick` is keyword-only in CPython (`data: Py_buffer / *`), so a
    // second positional argument is a TypeError (`test_uu` asserts
    // `b2a_uu(b"", True)` raises).
    if args.len() > 1 {
        return Err(type_error(format!(
            "b2a_uu() takes at most 1 positional argument ({} given)",
            args.len()
        )));
    }
    let data = buffer_bytes(args.first())?;
    let backtick = truthy_kw(kwargs, "backtick").unwrap_or(false);

    let bin_len = data.len();
    if bin_len > 45 {
        return Err(value_error("At most 45 bytes at once"));
    }

    let mut out = Vec::with_capacity(2 + bin_len.div_ceil(3) * 4);
    if backtick && bin_len == 0 {
        out.push(b'`');
    } else {
        out.push(b' ' + bin_len as u8);
    }

    let mut leftchar: u32 = 0;
    let mut leftbits: i32 = 0;
    let mut idx = 0usize;
    let mut remaining = bin_len as i64;
    while remaining > 0 || leftbits != 0 {
        if remaining > 0 {
            leftchar = (leftchar << 8) | u32::from(data[idx]);
        } else {
            leftchar <<= 8;
        }
        leftbits += 8;
        while leftbits >= 6 {
            let this_ch = ((leftchar >> (leftbits - 6)) & 0x3f) as u8;
            leftbits -= 6;
            if backtick && this_ch == 0 {
                out.push(b'`');
            } else {
                out.push(this_ch + b' ');
            }
        }
        remaining -= 1;
        idx += 1;
    }
    out.push(b'\n');
    Ok(Object::new_bytes(out))
}

// ---- quoted-printable (b2a_qp / a2b_qp) ----

fn is_qp_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn qp_hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'A'..=b'F' => c - b'A' + 10,
        b'a'..=b'f' => c - b'a' + 10,
        _ => 0,
    }
}

fn qp_to_hex(ch: u8) -> [u8; 2] {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    [H[(ch >> 4) as usize], H[(ch & 0x0f) as usize]]
}

/// Decode quoted-printable. Faithful port of `binascii_a2b_qp_impl`,
/// including the quirky soft-break handling for a lone `=\r` (everything up
/// to and including the next `\n` is dropped) and the "broken python qp"
/// `==` case.
fn a2b_qp_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_unknown_kwargs("a2b_qp", kwargs, &["data", "header"])?;
    let data_obj = positional_or_kw(args, kwargs, 0, "data");
    let data = input_bytes(data_obj.as_ref())?;
    let header = positional_or_kw(args, kwargs, 1, "header").is_some_and(|o| o.is_truthy());

    let n = data.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let c = data[i];
        if c == b'=' {
            i += 1;
            if i >= n {
                break;
            }
            let d = data[i];
            if d == b'\n' || d == b'\r' {
                // Soft line break. A bare '\r' (no following '\n') makes
                // CPython skip everything up to the next newline.
                if d != b'\n' {
                    while i < n && data[i] != b'\n' {
                        i += 1;
                    }
                }
                if i < n {
                    i += 1;
                }
            } else if d == b'=' {
                out.push(b'=');
                i += 1;
            } else if i + 1 < n && is_qp_hex(d) && is_qp_hex(data[i + 1]) {
                out.push((qp_hex_val(d) << 4) | qp_hex_val(data[i + 1]));
                i += 2;
            } else {
                // Not a valid escape: emit a literal '=' and reprocess the
                // following character on the next iteration (note: `i` is
                // intentionally left pointing at it).
                out.push(b'=');
            }
        } else if header && c == b'_' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    Ok(Object::new_bytes(out))
}

/// Encode quoted-printable. Faithful port of `binascii_b2a_qp_impl`: detect
/// CRLF vs bare-LF line endings up front and normalise to it, soft-wrap at
/// 76 columns, quote end-of-line whitespace, and honour
/// `quotetabs`/`istext`/`header`.
fn b2a_qp_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_unknown_kwargs("b2a_qp", kwargs, &["data", "quotetabs", "istext", "header"])?;
    let data_obj = positional_or_kw(args, kwargs, 0, "data");
    let data = buffer_bytes(data_obj.as_ref())?;
    let quotetabs = positional_or_kw(args, kwargs, 1, "quotetabs").is_some_and(|o| o.is_truthy());
    let istext = positional_or_kw(args, kwargs, 2, "istext").is_none_or(|o| o.is_truthy());
    let header = positional_or_kw(args, kwargs, 3, "header").is_some_and(|o| o.is_truthy());

    const MAXLINESIZE: i64 = 76;
    let n = data.len();
    // CRLF detection: the first '\n' immediately preceded by '\r' switches
    // the whole output to CRLF line ends (a CPython side effect).
    let crlf = data
        .iter()
        .position(|&b| b == b'\n')
        .is_some_and(|p| p > 0 && data[p - 1] == b'\r');

    let mut out: Vec<u8> = Vec::new();
    let mut linelen: i64 = 0;
    let mut i = 0usize;
    while i < n {
        let c = data[i];
        let needs_quote = c > 126
            || c == b'='
            || (header && c == b'_')
            || (c == b'.'
                && linelen == 0
                && (i + 1 == n
                    || data[i + 1] == b'\n'
                    || data[i + 1] == b'\r'
                    || data[i + 1] == 0))
            || (!istext && (c == b'\r' || c == b'\n'))
            || ((c == b'\t' || c == b' ') && i + 1 == n)
            || (c < 33 && c != b'\r' && c != b'\n' && (quotetabs || (c != b'\t' && c != b' ')));
        if needs_quote {
            if linelen + 3 >= MAXLINESIZE {
                out.push(b'=');
                if crlf {
                    out.push(b'\r');
                }
                out.push(b'\n');
                linelen = 0;
            }
            out.push(b'=');
            let h = qp_to_hex(c);
            out.push(h[0]);
            out.push(h[1]);
            i += 1;
            linelen += 3;
        } else if istext && (c == b'\n' || (i + 1 < n && c == b'\r' && data[i + 1] == b'\n')) {
            linelen = 0;
            // Protect against trailing whitespace by quoting the byte we
            // already emitted just before this newline.
            if let Some(&last) = out.last() {
                if last == b' ' || last == b'\t' {
                    let li = out.len() - 1;
                    out[li] = b'=';
                    let h = qp_to_hex(last);
                    out.push(h[0]);
                    out.push(h[1]);
                }
            }
            if crlf {
                out.push(b'\r');
            }
            out.push(b'\n');
            if c == b'\r' {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            if i + 1 != n && data[i + 1] != b'\n' && linelen + 1 >= MAXLINESIZE {
                out.push(b'=');
                if crlf {
                    out.push(b'\r');
                }
                out.push(b'\n');
                linelen = 0;
            }
            linelen += 1;
            if header && c == b' ' {
                out.push(b'_');
            } else {
                out.push(c);
            }
            i += 1;
        }
    }
    Ok(Object::new_bytes(out))
}

/// Fetch an argument that may be given positionally (at `pos`) or by keyword
/// (`name`), preferring the positional form.
fn positional_or_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
    pos: usize,
    name: &str,
) -> Option<Object> {
    args.get(pos).cloned().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
}

/// Reject any keyword not in `allowed` with a `TypeError`, matching the
/// CPython argument-clinic behaviour (`test_qp` feeds `b2a_qp(foo="bar")`
/// and `a2b_qp(**{1:1})` expecting `TypeError`).
fn reject_unknown_kwargs(
    func: &str,
    kwargs: &[(String, Object)],
    allowed: &[&str],
) -> Result<(), RuntimeError> {
    for (k, _) in kwargs {
        if !allowed.contains(&k.as_str()) {
            return Err(type_error(format!(
                "{func}() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    Ok(())
}
