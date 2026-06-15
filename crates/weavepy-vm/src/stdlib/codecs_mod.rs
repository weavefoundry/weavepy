//! `_codecs` — text codec engine (RFC 0019).
//!
//! Backed by `encoding_rs` for the multi-byte encodings (utf-16,
//! utf-32, cp1252, latin-1, etc.) and a hand-rolled UTF-8 path.
//! The frozen `codecs.py` builds the user-visible `lookup` /
//! `register` / `decode` / `encode` surface on top of this module.
//!
//! Surface here:
//!
//! * `encode(s, encoding, errors='strict')` — `str` -> `bytes`.
//! * `decode(b, encoding, errors='strict')` — `bytes` -> `str`.
//! * `lookup(name)` — returns a tuple of
//!   `(encoder, decoder, name, normalised_name, codepage_or_none)`.
//! * Module constants: `BOM_UTF8`, `BOM_UTF16`, `BOM_UTF16_LE`,
//!   `BOM_UTF16_BE`, `BOM_UTF32`, `BOM_UTF32_LE`, `BOM_UTF32_BE`.
//!
//! Error handlers covered: `strict`, `ignore`, `replace`,
//! `backslashreplace`, `xmlcharrefreplace`, `namereplace`,
//! `surrogateescape`, `surrogatepass`. Unknown handlers fall
//! through to `strict`.

use crate::sync::Rc;
use crate::sync::RefCell;

use encoding_rs::Encoding;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_codecs"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Encoding/decoding engine for the codecs module."),
        );
        register(&mut d, "encode", b_encode);
        register(&mut d, "decode", b_decode);
        register(&mut d, "lookup", b_lookup);
        register(&mut d, "utf_8_encode", b_utf8_encode);
        register(&mut d, "utf_8_decode", b_utf8_decode);
        register(&mut d, "utf_16_encode", b_utf16_encode);
        register(&mut d, "utf_16_decode", b_utf16_decode);
        register(&mut d, "utf_16_le_encode", b_utf16_le_encode);
        register(&mut d, "utf_16_le_decode", b_utf16_le_decode);
        register(&mut d, "utf_16_be_encode", b_utf16_be_encode);
        register(&mut d, "utf_16_be_decode", b_utf16_be_decode);
        register(&mut d, "utf_32_encode", b_utf32_encode);
        register(&mut d, "utf_32_decode", b_utf32_decode);
        register(&mut d, "utf_32_le_encode", b_utf32_le_encode);
        register(&mut d, "utf_32_le_decode", b_utf32_le_decode);
        register(&mut d, "utf_32_be_encode", b_utf32_be_encode);
        register(&mut d, "utf_32_be_decode", b_utf32_be_decode);
        register(&mut d, "ascii_encode", b_ascii_encode);
        register(&mut d, "ascii_decode", b_ascii_decode);
        register(&mut d, "latin_1_encode", b_latin1_encode);
        register(&mut d, "latin_1_decode", b_latin1_decode);
        register(&mut d, "cp1252_encode", b_cp1252_encode);
        register(&mut d, "cp1252_decode", b_cp1252_decode);
        register(
            &mut d,
            "raw_unicode_escape_encode",
            b_raw_unicode_escape_encode,
        );
        register(
            &mut d,
            "raw_unicode_escape_decode",
            b_raw_unicode_escape_decode,
        );
        register(&mut d, "unicode_escape_encode", b_unicode_escape_encode);
        register(&mut d, "unicode_escape_decode", b_unicode_escape_decode);

        d.insert(
            DictKey(Object::from_static("BOM")),
            Object::new_bytes(vec![0xEF, 0xBB, 0xBF]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF8")),
            Object::new_bytes(vec![0xEF, 0xBB, 0xBF]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF16")),
            Object::new_bytes(vec![0xFF, 0xFE]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF16_LE")),
            Object::new_bytes(vec![0xFF, 0xFE]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF16_BE")),
            Object::new_bytes(vec![0xFE, 0xFF]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF32")),
            Object::new_bytes(vec![0xFF, 0xFE, 0x00, 0x00]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF32_LE")),
            Object::new_bytes(vec![0xFF, 0xFE, 0x00, 0x00]),
        );
        d.insert(
            DictKey(Object::from_static("BOM_UTF32_BE")),
            Object::new_bytes(vec![0x00, 0x00, 0xFE, 0xFF]),
        );
    }
    Rc::new(PyModule {
        name: "_codecs".to_owned(),
        filename: None,
        dict,
    })
}

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    let bf = BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    };
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(bf)),
    );
}

// ---------- helpers ----------

fn arg_str(args: &[Object], idx: usize, name: &str) -> Result<String, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        _ => Err(type_error(format!(
            "{name}() argument {} must be str",
            idx + 1
        ))),
    }
}

fn arg_bytes(args: &[Object], idx: usize, name: &str) -> Result<Vec<u8>, RuntimeError> {
    match args.get(idx) {
        Some(o) => o
            .as_bytes_view()
            .ok_or_else(|| type_error(format!("{name}() argument {} must be bytes-like", idx + 1))),
        None => Err(type_error(format!("{name}() missing argument {}", idx + 1))),
    }
}

fn arg_errors(args: &[Object], idx: usize) -> String {
    match args.get(idx) {
        Some(Object::Str(s)) => s.to_string(),
        _ => "strict".to_owned(),
    }
}

/// Map a CPython-shaped encoding name to an `encoding_rs::Encoding`.
fn lookup_encoding(name: &str) -> Option<&'static Encoding> {
    let normalised: String = name
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-' && *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match normalised.as_str() {
        // Aliases that encoding_rs doesn't accept verbatim.
        "ascii" | "usascii" | "iso646us" | "646" => Encoding::for_label(b"us-ascii"),
        "latin1" | "latin" | "iso88591" | "88591" | "cp819" | "l1" => {
            Encoding::for_label(b"iso-8859-1")
        }
        // CPython `Lib/encodings/aliases.py` latin-N aliases.
        "latin2" | "l2" => Encoding::for_label(b"iso-8859-2"),
        "latin3" | "l3" => Encoding::for_label(b"iso-8859-3"),
        "latin4" | "l4" => Encoding::for_label(b"iso-8859-4"),
        "latin5" | "l5" => Encoding::for_label(b"iso-8859-9"),
        "latin6" | "l6" => Encoding::for_label(b"iso-8859-10"),
        "latin8" | "l8" => Encoding::for_label(b"iso-8859-14"),
        "latin9" | "l9" => Encoding::for_label(b"iso-8859-15"),
        "latin10" | "l10" => Encoding::for_label(b"iso-8859-16"),
        "utf8" | "u8" | "utf" => Encoding::for_label(b"utf-8"),
        "utf16" | "u16" => Encoding::for_label(b"utf-16"),
        "utf16le" => Encoding::for_label(b"utf-16le"),
        "utf16be" => Encoding::for_label(b"utf-16be"),
        "windows1252" | "cp1252" | "1252" => Encoding::for_label(b"windows-1252"),
        "macroman" => Encoding::for_label(b"macintosh"),
        "shiftjis" | "sjis" | "csshiftjis" => Encoding::for_label(b"shift_jis"),
        "gb2312" | "gbk" | "936" => Encoding::for_label(b"gbk"),
        "big5" | "csbig5" => Encoding::for_label(b"big5"),
        "euckr" | "ksc56011987" => Encoding::for_label(b"euc-kr"),
        "eucjp" | "ujis" => Encoding::for_label(b"euc-jp"),
        _ => Encoding::for_label(normalised.as_bytes()),
    }
}

// ---------- generic encode/decode dispatcher ----------

pub fn b_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = arg_str(args, 0, "encode")?;
    let encoding = arg_str(args, 1, "encode").unwrap_or_else(|_| "utf-8".to_owned());
    let errors = arg_errors(args, 2);
    let bytes = encode_str(&s, &encoding, &errors)?;
    Ok(Object::new_tuple(vec![
        Object::new_bytes(bytes),
        Object::Int(s.chars().count() as i64),
    ]))
}

pub fn b_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "decode")?;
    let encoding = arg_str(args, 1, "decode").unwrap_or_else(|_| "utf-8".to_owned());
    let errors = arg_errors(args, 2);
    let s = decode_bytes(&bytes, &encoding, &errors)?;
    let len = bytes.len() as i64;
    Ok(Object::new_tuple(vec![
        Object::from_str(s),
        Object::Int(len),
    ]))
}

fn b_lookup(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = arg_str(args, 0, "lookup")?;
    let enc = lookup_encoding(&name)
        .ok_or_else(|| crate::error::lookup_error(format!("unknown encoding: {name}")))?;
    let normalised = enc.name().to_lowercase();
    Ok(Object::from_str(normalised))
}

/// Known built-in error handler names. Custom handlers registered via
/// `codecs.register_error` live in the frozen `codecs.py` registry and
/// are resolved there before reaching the native engine.
const KNOWN_ERROR_HANDLERS: &[&str] = &[
    "strict",
    "ignore",
    "replace",
    "backslashreplace",
    "xmlcharrefreplace",
    "namereplace",
    "surrogateescape",
    "surrogatepass",
];

/// `-X dev`: validate the error-handler name eagerly, like CPython's
/// bpo-37388 check in `bytes(s, encoding, errors=…)` / `bytes.decode`.
/// Outside dev mode unknown handlers only fail if an error actually
/// occurs (matching CPython's lazy lookup).
fn check_error_handler(errors: &str) -> Result<(), RuntimeError> {
    if crate::vm_singletons::dev_mode() && !KNOWN_ERROR_HANDLERS.contains(&errors) {
        return Err(crate::error::lookup_error(format!(
            "unknown error handler name '{errors}'"
        )));
    }
    Ok(())
}

pub fn encode_str(s: &str, encoding: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    check_error_handler(errors)?;
    if let Some(out) = encode_special(s, encoding, errors)? {
        return Ok(out);
    }
    let enc = lookup_encoding(encoding)
        .ok_or_else(|| crate::error::lookup_error(format!("unknown encoding: {encoding}")))?;
    let (bytes, _, has_replacements) = enc.encode(s);
    if has_replacements && errors == "strict" {
        return Err(value_error(format!(
            "'{encoding}' codec can't encode input"
        )));
    }
    Ok(bytes.into_owned())
}

pub fn decode_bytes(bytes: &[u8], encoding: &str, errors: &str) -> Result<String, RuntimeError> {
    check_error_handler(errors)?;
    if let Some(out) = decode_special(bytes, encoding, errors)? {
        return Ok(out);
    }
    let enc = lookup_encoding(encoding)
        .ok_or_else(|| crate::error::lookup_error(format!("unknown encoding: {encoding}")))?;
    let (text, _, had_errors) = enc.decode(bytes);
    if had_errors && errors == "strict" {
        return Err(value_error(format!(
            "'{encoding}' codec can't decode input"
        )));
    }
    Ok(text.into_owned())
}

/// Handle special-case encodings whose semantics don't quite match
/// `encoding_rs`'s default behaviour (utf-8 with `surrogateescape`,
/// latin-1, raw_unicode_escape, etc.).
fn encode_special(s: &str, encoding: &str, errors: &str) -> Result<Option<Vec<u8>>, RuntimeError> {
    let key = encoding_key(encoding);
    Ok(match key.as_str() {
        "utf8" => Some(encode_utf8(s, errors)?),
        "ascii" => Some(encode_ascii(s, errors)?),
        "latin1" | "iso88591" => Some(encode_latin1(s, errors)?),
        "utf16" => Some(encode_utf16(s, false, true)),
        "utf16le" => Some(encode_utf16(s, false, false)),
        "utf16be" => Some(encode_utf16(s, true, false)),
        "utf32" => Some(encode_utf32(s, false, true)),
        "utf32le" => Some(encode_utf32(s, false, false)),
        "utf32be" => Some(encode_utf32(s, true, false)),
        "rawunicodeescape" => Some(encode_raw_unicode_escape(s)),
        "unicodeescape" => Some(encode_unicode_escape(s)),
        "cp437" | "437" | "ibm437" => Some(encode_cp437(s, errors)?),
        _ => None,
    })
}

fn decode_special(
    bytes: &[u8],
    encoding: &str,
    errors: &str,
) -> Result<Option<String>, RuntimeError> {
    let key = encoding_key(encoding);
    Ok(match key.as_str() {
        "utf8" => Some(decode_utf8(bytes, errors)?),
        "ascii" => Some(decode_ascii(bytes, errors)?),
        "latin1" | "iso88591" => Some(decode_latin1(bytes)),
        "utf16" => Some(decode_utf16(bytes, None)?),
        "utf16le" => Some(decode_utf16(bytes, Some(false))?),
        "utf16be" => Some(decode_utf16(bytes, Some(true))?),
        "utf32" => Some(decode_utf32(bytes, None)?),
        "utf32le" => Some(decode_utf32(bytes, Some(false))?),
        "utf32be" => Some(decode_utf32(bytes, Some(true))?),
        "rawunicodeescape" => Some(decode_raw_unicode_escape(bytes)?),
        "unicodeescape" => Some(decode_unicode_escape(bytes)?),
        "cp437" | "437" | "ibm437" => Some(decode_cp437(bytes)),
        _ => None,
    })
}

// ---------- cp437 (IBM PC / DOS codepage, not in encoding_rs) ----------

/// Upper half (0x80..=0xFF) of code page 437, from CPython's
/// `Lib/encodings/cp437.py` decoding table.
const CP437_HIGH: [char; 128] = [
    '\u{00c7}', '\u{00fc}', '\u{00e9}', '\u{00e2}', '\u{00e4}', '\u{00e0}', '\u{00e5}', '\u{00e7}',
    '\u{00ea}', '\u{00eb}', '\u{00e8}', '\u{00ef}', '\u{00ee}', '\u{00ec}', '\u{00c4}', '\u{00c5}',
    '\u{00c9}', '\u{00e6}', '\u{00c6}', '\u{00f4}', '\u{00f6}', '\u{00f2}', '\u{00fb}', '\u{00f9}',
    '\u{00ff}', '\u{00d6}', '\u{00dc}', '\u{00a2}', '\u{00a3}', '\u{00a5}', '\u{20a7}', '\u{0192}',
    '\u{00e1}', '\u{00ed}', '\u{00f3}', '\u{00fa}', '\u{00f1}', '\u{00d1}', '\u{00aa}', '\u{00ba}',
    '\u{00bf}', '\u{2310}', '\u{00ac}', '\u{00bd}', '\u{00bc}', '\u{00a1}', '\u{00ab}', '\u{00bb}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255d}', '\u{255c}', '\u{255b}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252c}', '\u{251c}', '\u{2500}', '\u{253c}', '\u{255e}', '\u{255f}',
    '\u{255a}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256c}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256b}',
    '\u{256a}', '\u{2518}', '\u{250c}', '\u{2588}', '\u{2584}', '\u{258c}', '\u{2590}', '\u{2580}',
    '\u{03b1}', '\u{00df}', '\u{0393}', '\u{03c0}', '\u{03a3}', '\u{03c3}', '\u{00b5}', '\u{03c4}',
    '\u{03a6}', '\u{0398}', '\u{03a9}', '\u{03b4}', '\u{221e}', '\u{03c6}', '\u{03b5}', '\u{2229}',
    '\u{2261}', '\u{00b1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00f7}', '\u{2248}',
    '\u{00b0}', '\u{2219}', '\u{00b7}', '\u{221a}', '\u{207f}', '\u{00b2}', '\u{25a0}', '\u{00a0}',
];

fn decode_cp437(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                CP437_HIGH[(b - 0x80) as usize]
            }
        })
        .collect()
}

fn encode_cp437(s: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if (c as u32) < 0x80 {
            out.push(c as u8);
        } else if let Some(pos) = CP437_HIGH.iter().position(|&h| h == c) {
            out.push(0x80 + pos as u8);
        } else {
            match errors {
                "ignore" => {}
                "replace" => out.push(b'?'),
                _ => {
                    return Err(crate::error::unicode_encode_error(
                        "charmap",
                        s,
                        i,
                        i + 1,
                        "character maps to <undefined>",
                    ))
                }
            }
        }
    }
    Ok(out)
}

fn encode_utf16(s: &str, big: bool, with_bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    if with_bom {
        if big {
            out.extend_from_slice(&[0xFE, 0xFF]);
        } else {
            out.extend_from_slice(&[0xFF, 0xFE]);
        }
    }
    let mut buf = [0u16; 2];
    for c in s.chars() {
        let u = c.encode_utf16(&mut buf);
        for code in u.iter() {
            let bytes = if big {
                code.to_be_bytes()
            } else {
                code.to_le_bytes()
            };
            out.extend_from_slice(&bytes);
        }
    }
    out
}

fn decode_utf16(bytes: &[u8], explicit_be: Option<bool>) -> Result<String, RuntimeError> {
    let (be, payload) = match explicit_be {
        Some(b) => (b, bytes),
        None => {
            if bytes.len() >= 2 {
                if bytes[..2] == [0xFF, 0xFE] {
                    (false, &bytes[2..])
                } else if bytes[..2] == [0xFE, 0xFF] {
                    (true, &bytes[2..])
                } else {
                    (false, bytes)
                }
            } else {
                (false, bytes)
            }
        }
    };
    if payload.len() % 2 != 0 {
        return Err(value_error("truncated utf-16 input"));
    }
    let mut codes: Vec<u16> = Vec::with_capacity(payload.len() / 2);
    let mut i = 0;
    while i < payload.len() {
        let bytes2 = [payload[i], payload[i + 1]];
        let code = if be {
            u16::from_be_bytes(bytes2)
        } else {
            u16::from_le_bytes(bytes2)
        };
        codes.push(code);
        i += 2;
    }
    String::from_utf16(&codes).map_err(|_| value_error("invalid utf-16 sequence"))
}

fn encode_utf32(s: &str, big: bool, with_bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 4 + 4);
    if with_bom {
        // Default BOM for non-explicit utf-32 is little-endian (CPython default).
        if big {
            out.extend_from_slice(&[0x00, 0x00, 0xFE, 0xFF]);
        } else {
            out.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x00]);
        }
    }
    for c in s.chars() {
        let cp = c as u32;
        let bytes = if big {
            cp.to_be_bytes()
        } else {
            cp.to_le_bytes()
        };
        out.extend_from_slice(&bytes);
    }
    out
}

fn decode_utf32(bytes: &[u8], explicit_be: Option<bool>) -> Result<String, RuntimeError> {
    let (be, payload) = match explicit_be {
        Some(b) => (b, bytes),
        None => {
            // Detect BOM.
            if bytes.len() >= 4 {
                if bytes[..4] == [0xFF, 0xFE, 0x00, 0x00] {
                    (false, &bytes[4..])
                } else if bytes[..4] == [0x00, 0x00, 0xFE, 0xFF] {
                    (true, &bytes[4..])
                } else {
                    (false, bytes) // assume little-endian like CPython.
                }
            } else {
                (false, bytes)
            }
        }
    };
    if payload.len() % 4 != 0 {
        return Err(value_error("truncated utf-32 input"));
    }
    let mut out = String::with_capacity(payload.len() / 4);
    let mut i = 0;
    while i < payload.len() {
        let chunk = &payload[i..i + 4];
        let cp = if be {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        } else {
            u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        };
        out.push(char::from_u32(cp).ok_or_else(|| value_error("invalid utf-32 codepoint"))?);
        i += 4;
    }
    Ok(out)
}

fn encoding_key(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-' && *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

// ---------- UTF-8 ----------

fn encode_utf8(s: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    if errors == "surrogateescape" {
        // Map U+DC80..U+DCFF back to 0x80..0xFF.
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            let cp = c as u32;
            if (0xDC80..=0xDCFF).contains(&cp) {
                out.push((cp - 0xDC00) as u8);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        Ok(out)
    } else {
        Ok(s.as_bytes().to_vec())
    }
}

fn decode_utf8(bytes: &[u8], errors: &str) -> Result<String, RuntimeError> {
    // Strict (and unknown-handler) failures raise a real
    // `UnicodeDecodeError` with CPython's payload and message shape:
    // `'utf-8' codec can't decode byte 0x80 in position 12: invalid
    // start byte`.
    let strict_err = |e: &std::str::Utf8Error| {
        let pos = e.valid_up_to();
        let end = pos + e.error_len().unwrap_or(1);
        let reason = if e.error_len().is_none() {
            "unexpected end of data"
        } else if bytes.get(pos).is_some_and(|b| (0x80..0xC2).contains(b)) {
            "invalid start byte"
        } else {
            "invalid continuation byte"
        };
        crate::error::unicode_decode_error("utf-8", bytes, pos, end.min(bytes.len()), reason)
    };
    match errors {
        "strict" => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|e| strict_err(&e)),
        "ignore" => Ok(String::from_utf8_lossy_lenient(bytes, false)),
        "replace" => Ok(String::from_utf8_lossy(bytes).into_owned()),
        "surrogateescape" => Ok(decode_utf8_surrogateescape(bytes)),
        "backslashreplace" => Ok(decode_utf8_backslashreplace(bytes)),
        _ => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|e| strict_err(&e)),
    }
}

fn decode_utf8_surrogateescape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match std::str::from_utf8(&bytes[i..]) {
            Ok(rest) => {
                out.push_str(rest);
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid]) });
                let bad_len = e.error_len().unwrap_or(1);
                for j in 0..bad_len {
                    let byte = bytes[i + valid + j];
                    // CPython maps the undecodable byte to the lone low
                    // surrogate U+DC00+byte. WeavePy's `str` is strict UTF-8
                    // (`Rc<str>`), which cannot hold surrogates, so we
                    // substitute U+FFFD rather than panic. Full
                    // surrogateescape round-tripping needs a surrogate-capable
                    // string representation (tracked separately).
                    let cp = 0xDC00 + u32::from(byte);
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                }
                i += valid + bad_len;
            }
        }
    }
    out
}

fn decode_utf8_backslashreplace(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match std::str::from_utf8(&bytes[i..]) {
            Ok(rest) => {
                out.push_str(rest);
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid]) });
                let bad_len = e.error_len().unwrap_or(1);
                for j in 0..bad_len {
                    out.push_str(&format!("\\x{:02x}", bytes[i + valid + j]));
                }
                i += valid + bad_len;
            }
        }
    }
    out
}

trait FromUtf8Lenient {
    fn from_utf8_lossy_lenient(bytes: &[u8], replace: bool) -> Self;
}

impl FromUtf8Lenient for String {
    fn from_utf8_lossy_lenient(bytes: &[u8], replace: bool) -> String {
        if replace {
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            // 'ignore' — silently skip invalid sequences.
            let mut out = String::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                match std::str::from_utf8(&bytes[i..]) {
                    Ok(rest) => {
                        out.push_str(rest);
                        break;
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        out.push_str(unsafe {
                            std::str::from_utf8_unchecked(&bytes[i..i + valid])
                        });
                        let bad_len = e.error_len().unwrap_or(1);
                        i += valid + bad_len;
                    }
                }
            }
            out
        }
    }
}

// ---------- ASCII / Latin-1 ----------

fn encode_ascii(s: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::with_capacity(s.len());
    for (pos, c) in s.chars().enumerate() {
        let cp = c as u32;
        if cp < 0x80 {
            out.push(cp as u8);
        } else {
            handle_encode_error(
                &mut out,
                s,
                pos,
                c,
                errors,
                "ascii",
                "ordinal not in range(128)",
            )?;
        }
    }
    Ok(out)
}

fn decode_ascii(bytes: &[u8], errors: &str) -> Result<String, RuntimeError> {
    let mut out = String::with_capacity(bytes.len());
    for (pos, &b) in bytes.iter().enumerate() {
        if b < 0x80 {
            out.push(b as char);
        } else {
            handle_decode_error(&mut out, bytes, pos, errors, "ascii")?;
        }
    }
    Ok(out)
}

fn encode_latin1(s: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::with_capacity(s.len());
    for (pos, c) in s.chars().enumerate() {
        let cp = c as u32;
        if cp < 0x100 {
            out.push(cp as u8);
        } else {
            handle_encode_error(
                &mut out,
                s,
                pos,
                c,
                errors,
                "latin-1",
                "ordinal not in range(256)",
            )?;
        }
    }
    Ok(out)
}

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn handle_encode_error(
    out: &mut Vec<u8>,
    source: &str,
    pos: usize,
    c: char,
    errors: &str,
    encoding: &str,
    reason: &str,
) -> Result<(), RuntimeError> {
    match errors {
        // Strict mode raises a real `UnicodeEncodeError` (a `ValueError`
        // subclass) carrying the canonical `(encoding, object, start, end,
        // reason)` payload, matching CPython — not a bare `ValueError`.
        "strict" => Err(crate::error::unicode_encode_error(
            encoding,
            source,
            pos,
            pos + 1,
            reason,
        )),
        "ignore" => Ok(()),
        "replace" => {
            out.push(b'?');
            Ok(())
        }
        "backslashreplace" => {
            let cp = c as u32;
            let s = if cp <= 0xFF {
                format!("\\x{:02x}", cp)
            } else if cp <= 0xFFFF {
                format!("\\u{:04x}", cp)
            } else {
                format!("\\U{:08x}", cp)
            };
            out.extend_from_slice(s.as_bytes());
            Ok(())
        }
        "namereplace" | "xmlcharrefreplace" => {
            let s = format!("&#{};", c as u32);
            out.extend_from_slice(s.as_bytes());
            Ok(())
        }
        _ => Err(value_error(format!("unknown error handler: {errors}"))),
    }
}

fn handle_decode_error(
    out: &mut String,
    input: &[u8],
    pos: usize,
    errors: &str,
    encoding: &str,
) -> Result<(), RuntimeError> {
    let byte = input[pos];
    match errors {
        "strict" => Err(crate::error::unicode_decode_error(
            encoding,
            input,
            pos,
            pos + 1,
            "ordinal not in range(128)",
        )),
        "ignore" => Ok(()),
        "replace" => {
            out.push('\u{FFFD}');
            Ok(())
        }
        "backslashreplace" => {
            out.push_str(&format!("\\x{byte:02x}"));
            Ok(())
        }
        "surrogateescape" => {
            // See `decode_utf8_surrogateescape`: the U+DC00+byte surrogate is
            // unrepresentable in a strict-UTF-8 `Rc<str>`, so fall back to
            // U+FFFD instead of panicking on `char::from_u32`.
            out.push(char::from_u32(0xDC00 + u32::from(byte)).unwrap_or('\u{FFFD}'));
            Ok(())
        }
        _ => Err(value_error(format!("unknown error handler: {errors}"))),
    }
}

// ---------- raw_unicode_escape / unicode_escape ----------

fn encode_raw_unicode_escape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x80 {
            out.push(cp as u8);
        } else if cp <= 0xFFFF {
            out.extend_from_slice(format!("\\u{:04x}", cp).as_bytes());
        } else {
            out.extend_from_slice(format!("\\U{:08x}", cp).as_bytes());
        }
    }
    out
}

fn decode_raw_unicode_escape(bytes: &[u8]) -> Result<String, RuntimeError> {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'u' if i + 6 <= bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 6])
                        .map_err(|_| value_error("bad raw_unicode_escape"))?;
                    let cp = u32::from_str_radix(hex, 16)
                        .map_err(|_| value_error("bad raw_unicode_escape"))?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += 6;
                    continue;
                }
                b'U' if i + 10 <= bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 10])
                        .map_err(|_| value_error("bad raw_unicode_escape"))?;
                    let cp = u32::from_str_radix(hex, 16)
                        .map_err(|_| value_error("bad raw_unicode_escape"))?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += 10;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

fn encode_unicode_escape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\'' => out.extend_from_slice(b"\\'"),
            '"' => out.extend_from_slice(b"\""),
            ch if (ch as u32) < 0x20 => {
                out.extend_from_slice(format!("\\x{:02x}", ch as u32).as_bytes());
            }
            ch if (ch as u32) < 0x80 => {
                out.push(ch as u8);
            }
            ch if (ch as u32) <= 0xFF => {
                out.extend_from_slice(format!("\\x{:02x}", ch as u32).as_bytes());
            }
            ch if (ch as u32) <= 0xFFFF => {
                out.extend_from_slice(format!("\\u{:04x}", ch as u32).as_bytes());
            }
            ch => {
                out.extend_from_slice(format!("\\U{:08x}", ch as u32).as_bytes());
            }
        }
    }
    out
}

fn decode_unicode_escape(bytes: &[u8]) -> Result<String, RuntimeError> {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'\\' => {
                    out.push('\\');
                    i += 2;
                }
                b'n' => {
                    out.push('\n');
                    i += 2;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                }
                b'\'' => {
                    out.push('\'');
                    i += 2;
                }
                b'"' => {
                    out.push('"');
                    i += 2;
                }
                b'a' => {
                    out.push('\x07');
                    i += 2;
                }
                b'b' => {
                    out.push('\x08');
                    i += 2;
                }
                b'f' => {
                    out.push('\x0C');
                    i += 2;
                }
                b'v' => {
                    out.push('\x0B');
                    i += 2;
                }
                b'0' => {
                    out.push('\0');
                    i += 2;
                }
                b'x' if i + 4 <= bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 4])
                        .map_err(|_| value_error("bad \\x escape"))?;
                    let cp =
                        u32::from_str_radix(hex, 16).map_err(|_| value_error("bad \\x escape"))?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += 4;
                }
                b'u' if i + 6 <= bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 6])
                        .map_err(|_| value_error("bad \\u escape"))?;
                    let cp =
                        u32::from_str_radix(hex, 16).map_err(|_| value_error("bad \\u escape"))?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += 6;
                }
                b'U' if i + 10 <= bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 10])
                        .map_err(|_| value_error("bad \\U escape"))?;
                    let cp =
                        u32::from_str_radix(hex, 16).map_err(|_| value_error("bad \\U escape"))?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += 10;
                }
                other => {
                    out.push('\\');
                    out.push(other as char);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(out)
}

// ---------- per-encoding wrapper functions used by the frozen layer ----------

macro_rules! enc_decoder {
    ($name:ident, $encoding:literal) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            // First arg is bytes, optional second arg is errors handler.
            let bytes = arg_bytes(args, 0, stringify!($name))?;
            let errors = arg_errors(args, 1);
            let s = decode_bytes(&bytes, $encoding, &errors)?;
            let len = bytes.len() as i64;
            Ok(Object::new_tuple(vec![
                Object::from_str(s),
                Object::Int(len),
            ]))
        }
    };
}

macro_rules! enc_encoder {
    ($name:ident, $encoding:literal) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            let s = arg_str(args, 0, stringify!($name))?;
            let errors = arg_errors(args, 1);
            let bytes = encode_str(&s, $encoding, &errors)?;
            let len = s.chars().count() as i64;
            Ok(Object::new_tuple(vec![
                Object::new_bytes(bytes),
                Object::Int(len),
            ]))
        }
    };
}

enc_encoder!(b_utf8_encode, "utf-8");
enc_decoder!(b_utf8_decode, "utf-8");
enc_encoder!(b_utf16_encode, "utf-16");
enc_decoder!(b_utf16_decode, "utf-16");
enc_encoder!(b_utf16_le_encode, "utf-16-le");
enc_decoder!(b_utf16_le_decode, "utf-16-le");
enc_encoder!(b_utf16_be_encode, "utf-16-be");
enc_decoder!(b_utf16_be_decode, "utf-16-be");
enc_encoder!(b_utf32_encode, "utf-32");
enc_decoder!(b_utf32_decode, "utf-32");
enc_encoder!(b_utf32_le_encode, "utf-32-le");
enc_decoder!(b_utf32_le_decode, "utf-32-le");
enc_encoder!(b_utf32_be_encode, "utf-32-be");
enc_decoder!(b_utf32_be_decode, "utf-32-be");
enc_encoder!(b_ascii_encode, "ascii");
enc_decoder!(b_ascii_decode, "ascii");
enc_encoder!(b_latin1_encode, "latin-1");
enc_decoder!(b_latin1_decode, "latin-1");
enc_encoder!(b_cp1252_encode, "cp1252");
enc_decoder!(b_cp1252_decode, "cp1252");
enc_encoder!(b_raw_unicode_escape_encode, "raw_unicode_escape");
enc_decoder!(b_raw_unicode_escape_decode, "raw_unicode_escape");
enc_encoder!(b_unicode_escape_encode, "unicode_escape");
enc_decoder!(b_unicode_escape_decode, "unicode_escape");
