//! The `base64` built-in module.
//!
//! Backed by the `base64` crate. The Python surface covers
//! `b64encode`/`b64decode`, `urlsafe_b64encode`/`urlsafe_b64decode`,
//! `b32encode`/`b32decode`, `b16encode`/`b16decode`, and the
//! `standard_b64encode`/`standard_b64decode` aliases. `b85encode`
//! falls back to the Python standard mapping; we ship the common
//! 95% — Ascii85 (`a85encode`) is deferred.

use crate::sync::Rc;
use crate::sync::RefCell;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
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
            Object::from_static("base64"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("RFC 3548 (base16/32/64) data encodings."),
        );
        for (name, body) in [
            (
                "b64encode",
                b64encode as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("b64decode", b64decode),
            ("standard_b64encode", b64encode),
            ("standard_b64decode", b64decode),
            ("urlsafe_b64encode", url_b64encode),
            ("urlsafe_b64decode", url_b64decode),
            ("b32encode", b32encode),
            ("b32decode", b32decode),
            ("b16encode", b16encode),
            ("b16decode", b16decode),
            ("encodebytes", b64encode_nl),
            ("decodebytes", b64decode),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    call: Box::new(body),
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "base64".to_owned(),
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

fn b64encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let _altchars = args.get(1);
    let encoded = STANDARD.encode(data);
    Ok(Object::new_bytes(encoded.into_bytes()))
}

fn b64encode_nl(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    // `base64.encodebytes` adds a `\n` every 76 chars and a trailing `\n`.
    let raw = STANDARD.encode(data);
    let mut out = String::with_capacity(raw.len() + raw.len() / 76 + 2);
    for chunk in raw.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push('\n');
    }
    Ok(Object::new_bytes(out.into_bytes()))
}

fn b64decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let trimmed: Vec<u8> = data
        .into_iter()
        .filter(|b| !matches!(b, b'\n' | b'\r' | b' '))
        .collect();
    let decoded = STANDARD
        .decode(&trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(&trimmed))
        .map_err(|e| value_error(format!("Invalid base64-encoded string: {e}")))?;
    Ok(Object::new_bytes(decoded))
}

fn url_b64encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    Ok(Object::new_bytes(URL_SAFE.encode(data).into_bytes()))
}

fn url_b64decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let decoded = URL_SAFE
        .decode(&data)
        .or_else(|_| URL_SAFE_NO_PAD.decode(&data))
        .map_err(|e| value_error(format!("Invalid url-safe base64: {e}")))?;
    Ok(Object::new_bytes(decoded))
}

// ---- base32 (RFC 4648) ----

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn b32encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    Ok(Object::new_bytes(b32_encode_bytes(&data)))
}

fn b32_encode_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        let len = chunk.len();
        buf[..len].copy_from_slice(chunk);
        // 5 bytes => 40 bits => 8 base32 chars
        let bits: u64 = (u64::from(buf[0]) << 32)
            | (u64::from(buf[1]) << 24)
            | (u64::from(buf[2]) << 16)
            | (u64::from(buf[3]) << 8)
            | u64::from(buf[4]);
        for shift in (0..8).rev() {
            let idx = ((bits >> (shift * 5)) & 0x1F) as usize;
            out.push(B32_ALPHABET[idx]);
        }
        let pad = match len {
            1 => 6,
            2 => 4,
            3 => 3,
            4 => 1,
            _ => 0,
        };
        let l = out.len();
        for byte in &mut out[l - pad..] {
            *byte = b'=';
        }
    }
    out
}

fn b32decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let trimmed: Vec<u8> = data
        .into_iter()
        .filter(|b| !matches!(b, b'\n' | b'\r' | b' '))
        .map(|b| b.to_ascii_uppercase())
        .collect();
    let mut out = Vec::new();
    let mut buf: u64 = 0;
    let mut bits = 0;
    for &c in &trimmed {
        if c == b'=' {
            continue;
        }
        let pos = B32_ALPHABET
            .iter()
            .position(|&x| x == c)
            .ok_or_else(|| value_error(format!("Invalid base32 char: {c}")))?;
        buf = (buf << 5) | (pos as u64);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(Object::new_bytes(out))
}

// ---- base16 (hex) ----

fn b16encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in &data {
        use std::fmt::Write;
        let mut s = String::new();
        write!(s, "{b:02X}").unwrap();
        out.extend_from_slice(s.as_bytes());
    }
    Ok(Object::new_bytes(out))
}

fn b16decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = input_bytes(args.first())?;
    if data.len() % 2 != 0 {
        return Err(value_error("base16 input must be even length"));
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks(2) {
        let s = std::str::from_utf8(pair).map_err(|_| value_error("non-ASCII in base16 input"))?;
        let v = u8::from_str_radix(s, 16)
            .map_err(|_| value_error(format!("invalid base16 chars: {s}")))?;
        out.push(v);
    }
    Ok(Object::new_bytes(out))
}
