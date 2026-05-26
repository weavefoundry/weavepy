//! The `binascii` built-in module.
//!
//! The CPython `binascii` API is a grab-bag of byte-level utilities
//! that grew up around uuencode / yEnc / quoted-printable / CRC-32.
//! We ship the modern-day subset everyday programs actually use:
//!
//! * `b2a_hex` / `hexlify` / `a2b_hex` / `unhexlify`
//! * `b2a_base64` / `a2b_base64` (used by stdlib `email` to
//!   decode MIME base64 chunks)
//! * `crc32`
//! * `Error` (alias for `ValueError`)

use crate::sync::Rc;
use crate::sync::RefCell;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

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
        for (name, body) in [
            (
                "b2a_hex",
                b2a_hex as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("hexlify", b2a_hex),
            ("a2b_hex", a2b_hex),
            ("unhexlify", a2b_hex),
            ("b2a_base64", b2a_base64),
            ("a2b_base64", a2b_base64),
            ("crc32", crc32),
            ("crc_hqx", crc_hqx),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "binascii".to_owned(),
        filename: None,
        dict,
    })
}

fn input_bytes(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::Str(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(type_error("expected bytes-like object")),
    }
}

fn b2a_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in &data {
        use std::fmt::Write;
        let mut s = String::new();
        write!(s, "{b:02x}").unwrap();
        out.extend_from_slice(s.as_bytes());
    }
    Ok(Object::new_bytes(out))
}

fn a2b_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    if data.len() % 2 != 0 {
        return Err(value_error("odd-length hex string"));
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks(2) {
        let s = std::str::from_utf8(pair).map_err(|_| value_error("non-ASCII in hex string"))?;
        let v =
            u8::from_str_radix(s, 16).map_err(|_| value_error(format!("non-hex chars: {s}")))?;
        out.push(v);
    }
    Ok(Object::new_bytes(out))
}

fn b2a_base64(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let newline = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        _ => true,
    };
    let mut s = STANDARD.encode(data).into_bytes();
    if newline {
        s.push(b'\n');
    }
    Ok(Object::new_bytes(s))
}

fn a2b_base64(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let trimmed: Vec<u8> = data
        .into_iter()
        .filter(|b| !matches!(b, b'\n' | b'\r' | b' '))
        .collect();
    let decoded = STANDARD
        .decode(&trimmed)
        .map_err(|e| value_error(format!("Invalid base64: {e}")))?;
    Ok(Object::new_bytes(decoded))
}

fn crc32(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
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
    // implement the canonical polynomial for completeness.
    let data = input_bytes(args.first())?;
    let init = match args.get(1) {
        Some(Object::Int(n)) => *n as u16,
        None | Some(Object::None) => 0,
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
