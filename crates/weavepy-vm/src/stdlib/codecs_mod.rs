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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        register(&mut d, "utf_7_encode", b_utf7_encode);
        register(&mut d, "utf_7_decode", b_utf7_decode);
        register(&mut d, "utf_16_ex_decode", b_utf16_ex_decode);
        register(&mut d, "utf_32_ex_decode", b_utf32_ex_decode);
        register(&mut d, "readbuffer_encode", b_readbuffer_encode);
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

        // RFC 0063 WS6 — Windows-only code-page codecs, exactly the surface
        // CPython's `_codecs` grows on win32 (`encodings/mbcs.py` and
        // `encodings/oem.py` import these through `codecs`).
        #[cfg(windows)]
        {
            register(&mut d, "code_page_encode", nt_code_page::b_code_page_encode);
            register(&mut d, "code_page_decode", nt_code_page::b_code_page_decode);
            register(&mut d, "mbcs_encode", nt_code_page::b_mbcs_encode);
            register(&mut d, "mbcs_decode", nt_code_page::b_mbcs_decode);
            register(&mut d, "oem_encode", nt_code_page::b_oem_encode);
            register(&mut d, "oem_decode", nt_code_page::b_oem_decode);
        }

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

/// Coerce a *codec-name* argument the way CPython's `s` argument parser does
/// in `_codecs.lookup`/`encode`/`decode`: a `str` passes through, but a
/// lone-surrogate `str` (WeavePy [`Object::WStr`]) cannot be UTF-8-encoded for
/// the C codec name, so it raises `UnicodeEncodeError` rather than a
/// `LookupError` for a replacement-char name (`test_io.test_constructor` /
/// `test_reconfigure_errors`). Anything non-string is a `TypeError`.
fn arg_codec_name(args: &[Object], idx: usize, name: &str) -> Result<String, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(Object::WStr(cps)) => {
            // A genuine `WStr` always carries a lone surrogate, so the strict
            // UTF-8 encoder raises `UnicodeEncodeError`; propagate it. The
            // trailing `Ok` is a defensive fallback that never runs in practice.
            encode_codepoints(cps, "utf-8", "strict")?;
            Ok(cps.iter().filter_map(|&c| char::from_u32(c)).collect())
        }
        _ => Err(type_error(format!(
            "{name}() argument {} must be str",
            idx + 1
        ))),
    }
}

fn arg_bytes(args: &[Object], idx: usize, name: &str) -> Result<Vec<u8>, RuntimeError> {
    let _ = name;
    match args.get(idx) {
        // Argument-clinic `Py_buffer` converter wording (`codecs.utf_8_decode("x")`
        // → "a bytes-like object is required, not 'str'").
        Some(o) => o.as_bytes_view().ok_or_else(|| {
            type_error(format!(
                "a bytes-like object is required, not '{}'",
                o.type_name()
            ))
        }),
        None => Err(type_error(format!("{name}() missing argument {}", idx + 1))),
    }
}

/// Buffer *or* `str` argument (CPython's `s*` converter: a `str` argument
/// yields its UTF-8 bytes) — `unicode_escape_decode` and friends accept both.
fn arg_bytes_or_str_utf8(args: &[Object], idx: usize, name: &str) -> Result<Vec<u8>, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.as_bytes().to_vec()),
        _ => arg_bytes(args, idx, name),
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
    // CPython's `encodings.normalize_encoding` keeps only ASCII
    // alphanumerics (non-ASCII chars act as separators and are dropped), so
    // e.g. 'utf-8”' still resolves to utf-8.
    let normalised: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
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
        // The CJK codecs live in frozen Python modules with CPython-parity
        // tables and state machines (RFC 0050 WS3: `_codec_cjk_dbcs`,
        // `_codec_cjk_ext`, `_codec_euc_jis_2004`). `encoding_rs` must never
        // claim them: WHATWG's shift_jis index IS Windows-31J (cp932), its
        // euc-kr IS the unified-hangul cp949, its big5 carries the HKSCS
        // extensions, its iso-2022-jp state machine differs from CPython's,
        // and it maps iso-2022-kr / hz labels onto the data-destroying
        // "replacement" encoding. The catch-all `for_label` below is
        // post-filtered for the same reason (labels like "csksc56011987" or
        // "xsjis" also resolve to WHATWG CJK indices).
        "iso2022jp" | "csiso2022jp" | "iso2022kr" | "csiso2022kr" | "hzgb2312" => None,
        // KOI8 Cyrillic — `encoding_rs` knows these, but only under the
        // hyphenated WHATWG labels our normaliser strips.
        "koi8r" | "cskoi8r" => Encoding::for_label(b"koi8-r"),
        "koi8u" => Encoding::for_label(b"koi8-u"),
        _ => Encoding::for_label(normalised.as_bytes()).filter(|enc| {
            !matches!(
                enc.name(),
                "Shift_JIS"
                    | "EUC-JP"
                    | "EUC-KR"
                    | "GBK"
                    | "gb18030"
                    | "Big5"
                    | "ISO-2022-JP"
                    | "replacement"
            )
        }),
    }
}

// ---------- single-byte (charmap) codec tables ----------

/// Decode table (byte → code point, `CHARMAP_UNDEFINED` = unmapped) for a
/// single-byte codec, built lazily from `encoding_rs`'s index and cached
/// forever. `cp437` (absent from WHATWG) and `cp1252` (whose WHATWG index
/// wrongly fills CPython's five undefined slots) come from the local
/// CPython-shaped tables instead.
fn sbcs_decode_table(encoding: &str) -> Option<&'static [u32; 256]> {
    use crate::stdlib::codecs_engine::CHARMAP_UNDEFINED;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    let key = encoding_key(encoding);
    static CACHE: OnceLock<Mutex<HashMap<String, &'static [u32; 256]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(t) = cache.lock().unwrap().get(&key) {
        return Some(*t);
    }
    let mut table = [CHARMAP_UNDEFINED; 256];
    for (i, slot) in table.iter_mut().take(0x80).enumerate() {
        *slot = i as u32;
    }
    match key.as_str() {
        "cp437" | "437" | "ibm437" => {
            for (i, &c) in CP437_HIGH.iter().enumerate() {
                table[0x80 + i] = c as u32;
            }
        }
        "cp1252" | "windows1252" | "1252" => {
            for (i, &c) in CP1252_HIGH.iter().enumerate() {
                if let Some(c) = c {
                    table[0x80 + i] = c as u32;
                }
            }
        }
        // `ascii`/`latin1` have dedicated engine codecs (and their WHATWG
        // labels alias the lenient windows-1252 superset, so the generic
        // path below must never see them).
        "ascii" | "latin1" | "iso88591" => return None,
        _ => {
            let enc = lookup_encoding(encoding)?;
            // Reject non-single-byte codecs, and any label WHATWG smears
            // onto windows-1252 (CPython's cp1252 differs — handled above).
            if !enc.is_single_byte() || enc == encoding_rs::WINDOWS_1252 {
                return None;
            }
            for b in 0x80..=0xFFu8 {
                if let Some(s) = enc.decode_without_bom_handling_and_without_replacement(&[b]) {
                    let mut chars = s.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        table[b as usize] = c as u32;
                    }
                }
            }
        }
    }
    let leaked: &'static [u32; 256] = Box::leak(Box::new(table));
    cache.lock().unwrap().insert(key, leaked);
    Some(leaked)
}

// ---------- generic encode/decode dispatcher ----------

pub fn b_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args
        .first()
        .ok_or_else(|| type_error("encode() missing argument 1"))?;
    // Accept both string representations (and built-in str subclass instances)
    // so a surrogate-bearing `WStr` encodes through the WTF-8 path.
    let encoding = arg_str(args, 1, "encode").unwrap_or_else(|_| "utf-8".to_owned());
    let errors = arg_errors(args, 2);
    let nchars = obj.len().unwrap_or(0) as i64;
    let bytes = encode_obj(obj, &encoding, &errors)?;
    Ok(Object::new_tuple(vec![
        Object::new_bytes(bytes),
        Object::Int(nchars),
    ]))
}

pub fn b_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "decode")?;
    let encoding = arg_str(args, 1, "decode").unwrap_or_else(|_| "utf-8".to_owned());
    let errors = arg_errors(args, 2);
    let s = decode_bytes_obj(&bytes, &encoding, &errors)?;
    let len = bytes.len() as i64;
    Ok(Object::new_tuple(vec![s, Object::Int(len)]))
}

fn b_lookup(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = arg_codec_name(args, 0, "lookup")?;
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

/// Public wrapper used by the `io` text layer: CPython's `TextIOWrapper`
/// validates the `errors=` handler eagerly when `_CHECK_ERRORS` is set
/// (debug builds or `-X dev`), so `open(..., errors='Boom')` raises
/// `LookupError` at construction (`test_io.test_check_encoding_errors`).
pub(crate) fn check_text_errors(errors: &str) -> Result<(), RuntimeError> {
    check_error_handler(errors)
}

/// For a BOM-prefixing encoding (byte-order-less `utf-16`/`utf-32`, or
/// `utf-8-sig`), return the **continuation** codec used after the BOM has been
/// emitted once — the BOM-less variant. CPython's incremental encoders write
/// the BOM exactly once at the start of the stream, then switch to the native
/// byte-order codec; both the native `PyFile` text path and `io.TextIOWrapper`
/// reproduce that with a start-of-stream flag plus this mapping. Returns `None`
/// for codecs that never emit a BOM (their writes are stateless).
pub fn bom_continuation(encoding: &str) -> Option<&'static str> {
    match encoding_key(encoding).as_str() {
        // WeavePy encodes byte-order-less utf-16/utf-32 as little-endian (its
        // x86_64/aarch64 targets), so the continuation is the LE codec.
        "utf16" => Some("utf-16-le"),
        "utf32" => Some("utf-32-le"),
        "utf8sig" => Some("utf-8"),
        _ => None,
    }
}

pub fn encode_str(s: &str, encoding: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    check_error_handler(errors)?;
    if let Some(out) = encode_special(s, encoding, errors)? {
        return Ok(out);
    }
    if let Some(enc) = lookup_encoding(encoding) {
        let (bytes, _, has_replacements) = enc.encode(s);
        if has_replacements && errors == "strict" {
            return Err(value_error(format!(
                "'{encoding}' codec can't encode input"
            )));
        }
        return Ok(bytes.into_owned());
    }
    // Native fast path doesn't know this encoding — consult the Python codec
    // registry (custom `codecs.register` codecs and the `encodings/*.py`
    // modules), mirroring CPython's C-fast-path/Python-registry split.
    if let Some(out) = encode_via_registry(s, encoding, errors)? {
        return Ok(out);
    }
    Err(crate::error::lookup_error(format!(
        "unknown encoding: {encoding}"
    )))
}

/// Encode any string-bearing `Object` (`str` or surrogate-bearing `WStr`).
/// The `WStr` path routes through [`encode_codepoints`] so lone surrogates are
/// handled by `surrogateescape`/`surrogatepass`; a plain `Str` keeps the
/// existing UTF-8 fast path.
pub fn encode_obj(obj: &Object, encoding: &str, errors: &str) -> Result<Vec<u8>, RuntimeError> {
    match obj {
        Object::WStr(cps) => encode_codepoints(cps, encoding, errors),
        Object::Str(s) => encode_str(s, encoding, errors),
        // Built-in str subclass instance, etc. — fall back to its text view.
        other => encode_str(&other.to_str(), encoding, errors),
    }
}

/// Encode a code-point sequence (each entry a Unicode scalar value *or* a lone
/// surrogate) to bytes. This is the surrogate-aware counterpart of
/// [`encode_str`]: it implements `surrogateescape`/`surrogatepass` natively for
/// the UTF and charmap codecs so PEP 383 paths and `surrogatepass` round-trip.
pub fn encode_codepoints(
    cps: &[u32],
    encoding: &str,
    errors: &str,
) -> Result<Vec<u8>, RuntimeError> {
    check_error_handler(errors)?;
    // No surrogate present (canonicalisation should normally prevent a `WStr`
    // here, but a raw caller may pass scalars): reuse the `str` fast path.
    if !cps.iter().any(|&c| (0xD800..=0xDFFF).contains(&c)) {
        let s: String = cps.iter().filter_map(|&c| char::from_u32(c)).collect();
        return encode_str(&s, encoding, errors);
    }
    let key = encoding_key(encoding);
    use crate::stdlib::codecs_engine as engine;
    let out = match key.as_str() {
        "utf8" => engine::utf8_encode(cps, errors)?,
        "utf8sig" => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend(engine::utf8_encode(cps, errors)?);
            out
        }
        "ascii" => engine::ascii_encode(cps, errors)?,
        "latin1" | "iso88591" => engine::latin1_encode(cps, errors)?,
        "utf16le" => engine::utf16_encode(cps, errors, -1)?,
        "utf16be" => engine::utf16_encode(cps, errors, 1)?,
        "utf16" => engine::utf16_encode(cps, errors, 0)?,
        "utf32le" => engine::utf32_encode(cps, errors, -1)?,
        "utf32be" => engine::utf32_encode(cps, errors, 1)?,
        "utf32" => engine::utf32_encode(cps, errors, 0)?,
        "utf7" => engine::utf7_encode(cps, errors)?,
        // The escape codecs operate on raw code points, so lone surrogates
        // are representable (CPython encodes '\udfff' as the escape bytes).
        "rawunicodeescape" => {
            let mut out = Vec::with_capacity(cps.len());
            for &cp in cps {
                if cp < 0x100 {
                    out.push(cp as u8);
                } else if cp <= 0xFFFF {
                    out.extend_from_slice(format!("\\u{cp:04x}").as_bytes());
                } else {
                    out.extend_from_slice(format!("\\U{cp:08x}").as_bytes());
                }
            }
            out
        }
        "unicodeescape" => {
            let mut out = Vec::new();
            for &cp in cps {
                match char::from_u32(cp) {
                    Some(ch) => out.extend(encode_unicode_escape(&ch.to_string())),
                    None => out.extend_from_slice(format!("\\u{cp:04x}").as_bytes()),
                }
            }
            out
        }
        _ if sbcs_decode_table(encoding).is_some() => {
            let table = sbcs_decode_table(encoding).expect("just checked");
            engine::charmap_encode_table(cps, errors, table)?
        }
        _ => {
            // Registry-backed codec (CJK, custom `codecs.register` codecs):
            // hand the surrogate-bearing string to the Python codec whole so
            // its own error-handler protocol runs (CPython never pre-screens
            // surrogates for these).
            if sbcs_decode_table(encoding).is_none() && lookup_encoding(encoding).is_none() {
                if let Some(out) =
                    encode_via_registry_obj(Object::WStr(cps.into()), encoding, errors)?
                {
                    return Ok(out);
                }
            }
            // Any other codec: encode maximal scalar runs through the normal
            // string engine and resolve each lone surrogate via the error
            // handler (`surrogatepass` is invalid for non-UTF codecs, so it
            // falls through to a `UnicodeEncodeError`, matching CPython).
            let mut out = Vec::new();
            let mut run = String::new();
            let flush = |run: &mut String, out: &mut Vec<u8>| -> Result<(), RuntimeError> {
                if !run.is_empty() {
                    out.extend(encode_str(run, encoding, errors)?);
                    run.clear();
                }
                Ok(())
            };
            for (i, &cp) in cps.iter().enumerate() {
                if let Some(ch) = char::from_u32(cp) {
                    run.push(ch);
                } else {
                    flush(&mut run, &mut out)?;
                    match errors {
                        "surrogateescape" if (0xDC80..=0xDCFF).contains(&cp) => {
                            out.push((cp - 0xDC00) as u8);
                        }
                        "ignore" => {}
                        "replace" => out.push(b'?'),
                        "backslashreplace" => {
                            out.extend_from_slice(format!("\\u{cp:04x}").as_bytes())
                        }
                        "xmlcharrefreplace" => out.extend_from_slice(format!("&#{cp};").as_bytes()),
                        _ => return Err(surrogate_encode_error(encoding, cps, i)),
                    }
                }
            }
            flush(&mut run, &mut out)?;
            out
        }
    };
    Ok(out)
}

/// `UnicodeEncodeError` for a lone surrogate at `pos` in a code-point sequence.
/// The `.object` attribute keeps the surrogate-bearing `WStr` so the message
/// names the real offending code point; type/positions/reason match CPython.
fn surrogate_encode_error(encoding: &str, cps: &[u32], pos: usize) -> RuntimeError {
    crate::error::unicode_encode_error_obj(
        encoding,
        Object::WStr(cps.into()),
        pos,
        pos + 1,
        "surrogates not allowed",
    )
}

/// Decode bytes to a string `Object`, producing a surrogate-bearing [`WStr`]
/// when the codec + error handler yields lone surrogates (PEP 383
/// `surrogateescape`, `surrogatepass`), or a plain [`Object::Str`] otherwise.
pub fn decode_bytes_obj(
    bytes: &[u8],
    encoding: &str,
    errors: &str,
) -> Result<Object, RuntimeError> {
    check_error_handler(errors)?;
    // The UTF/ASCII/Latin-1 family routes through the unified engine
    // (RFC 0050 WS2): every error handler — built-in *and* custom via
    // `codecs.register_error` — resolves uniformly, and surrogate-producing
    // handlers yield a `WStr` transparently.
    use crate::stdlib::codecs_engine as engine;
    let key = encoding_key(encoding);
    match key.as_str() {
        "utf8" => return Ok(engine::utf8_decode(bytes, errors, true)?.0),
        "utf8sig" => {
            let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
            return Ok(engine::utf8_decode(body, errors, true)?.0);
        }
        "ascii" => return Ok(engine::ascii_decode(bytes, errors)?.0),
        "latin1" | "iso88591" => return Ok(engine::latin1_decode(bytes, errors)?.0),
        "utf16" => return Ok(engine::utf16_decode(bytes, errors, 0, true)?.0),
        "utf16le" => return Ok(engine::utf16_decode(bytes, errors, -1, true)?.0),
        "utf16be" => return Ok(engine::utf16_decode(bytes, errors, 1, true)?.0),
        "utf32" => return Ok(engine::utf32_decode(bytes, errors, 0, true)?.0),
        "utf32le" => return Ok(engine::utf32_decode(bytes, errors, -1, true)?.0),
        "utf32be" => return Ok(engine::utf32_decode(bytes, errors, 1, true)?.0),
        "utf7" => return Ok(engine::utf7_decode(bytes, errors, true)?.0),
        "unicodeescape" => {
            let (obj, _, warn) = engine::unicode_escape_decode(bytes, errors, true)?;
            if let Some(msg) = warn {
                emit_deprecation(&msg)?;
            }
            return Ok(obj);
        }
        "rawunicodeescape" => return Ok(engine::raw_unicode_escape_decode(bytes, errors, true)?.0),
        _ => {
            // Single-byte codecs route through the charmap engine so
            // custom handlers and surrogate-producing handlers work.
            if let Some(table) = sbcs_decode_table(encoding) {
                return Ok(engine::charmap_decode_table(bytes, errors, table)?.0);
            }
        }
    }
    if let Some(cps) = decode_special_codepoints(bytes, encoding, errors)? {
        return Ok(Object::str_from_codepoints(cps));
    }
    // No surrogate-producing path applies: decode to a plain UTF-8 string.
    Ok(Object::from_str(decode_bytes(bytes, encoding, errors)?))
}

/// Surrogate-producing decoders. Returns `Some(code points)` for the
/// (encoding, errors) combinations that can yield lone surrogates, else `None`
/// so [`decode_bytes_obj`] uses the plain string path.
fn decode_special_codepoints(
    bytes: &[u8],
    encoding: &str,
    errors: &str,
) -> Result<Option<Vec<u32>>, RuntimeError> {
    let key = encoding_key(encoding);
    let cps = match (key.as_str(), errors) {
        ("utf8" | "utf8sig", "surrogateescape") => {
            let body = if key == "utf8sig" {
                bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes)
            } else {
                bytes
            };
            decode_utf8_surrogateescape_codepoints(body)
        }
        ("utf8" | "utf8sig", "surrogatepass") => {
            let body = if key == "utf8sig" {
                bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes)
            } else {
                bytes
            };
            decode_utf8_surrogatepass_codepoints(body)?
        }
        ("ascii", "surrogateescape") => {
            let mut out = Vec::with_capacity(bytes.len());
            for &b in bytes {
                if b < 0x80 {
                    out.push(u32::from(b));
                } else {
                    out.push(0xDC00 + u32::from(b));
                }
            }
            out
        }
        ("utf16le" | "utf16be" | "utf16", "surrogatepass") => {
            let big = match key.as_str() {
                "utf16be" => Some(true),
                "utf16le" => Some(false),
                _ => None,
            };
            decode_utf16_surrogatepass_codepoints(bytes, big)?
        }
        ("utf32le" | "utf32be" | "utf32", "surrogatepass") => {
            let big = match key.as_str() {
                "utf32be" => Some(true),
                "utf32le" => Some(false),
                _ => None,
            };
            decode_utf32_surrogatepass_codepoints(bytes, big)?
        }
        _ => return Ok(None),
    };
    Ok(Some(cps))
}

/// UTF-8 `surrogateescape` decode to code points: each undecodable byte becomes
/// the lone low surrogate U+DC00+byte (PEP 383).
fn decode_utf8_surrogateescape_codepoints(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match std::str::from_utf8(&bytes[i..]) {
            Ok(rest) => {
                out.extend(rest.chars().map(|c| c as u32));
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                let good = unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid]) };
                out.extend(good.chars().map(|c| c as u32));
                let bad_len = e.error_len().unwrap_or(1);
                for j in 0..bad_len {
                    out.push(0xDC00 + u32::from(bytes[i + valid + j]));
                }
                i += valid + bad_len;
            }
        }
    }
    out
}

/// UTF-8 `surrogatepass` decode to code points: like strict UTF-8 but the
/// three-byte sequences `ED A0..BF 80..BF` decode to the encoded lone surrogate
/// rather than raising.
fn decode_utf8_surrogatepass_codepoints(bytes: &[u8]) -> Result<Vec<u32>, RuntimeError> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(u32::from(b));
            i += 1;
        } else if (0xED..=0xED).contains(&b)
            && i + 2 < bytes.len()
            && (0xA0..=0xBF).contains(&bytes[i + 1])
            && (0x80..=0xBF).contains(&bytes[i + 2])
        {
            // Encoded lone surrogate (U+D800..U+DFFF).
            let cp = ((u32::from(b) & 0x0F) << 12)
                | ((u32::from(bytes[i + 1]) & 0x3F) << 6)
                | (u32::from(bytes[i + 2]) & 0x3F);
            out.push(cp);
            i += 3;
        } else {
            // Decode a normal UTF-8 scalar starting here.
            match std::str::from_utf8(&bytes[i..]) {
                Ok(rest) => {
                    out.extend(rest.chars().map(|c| c as u32));
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        let good = unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid]) };
                        out.extend(good.chars().map(|c| c as u32));
                        i += valid;
                    } else {
                        return Err(crate::error::unicode_decode_error(
                            "utf-8",
                            bytes,
                            i,
                            i + 1,
                            "invalid start byte",
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// UTF-16 `surrogatepass` decode: unpaired surrogates pass through as their own
/// code point instead of raising.
fn decode_utf16_surrogatepass_codepoints(
    bytes: &[u8],
    big: Option<bool>,
) -> Result<Vec<u32>, RuntimeError> {
    let (big, body) = resolve_utf16_bom(bytes, big);
    if body.len() % 2 != 0 {
        return Err(crate::error::unicode_decode_error(
            "utf-16",
            bytes,
            body.len() - 1,
            body.len(),
            "truncated data",
        ));
    }
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| {
            if big {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xD800..=0xDBFF).contains(&u)
            && i + 1 < units.len()
            && (0xDC00..=0xDFFF).contains(&units[i + 1])
        {
            let hi = u32::from(u) - 0xD800;
            let lo = u32::from(units[i + 1]) - 0xDC00;
            out.push(0x1_0000 + (hi << 10) + lo);
            i += 2;
        } else {
            // Scalar or lone surrogate — surrogatepass keeps it verbatim.
            out.push(u32::from(u));
            i += 1;
        }
    }
    Ok(out)
}

/// UTF-32 `surrogatepass` decode: 32-bit code units, surrogate values allowed.
fn decode_utf32_surrogatepass_codepoints(
    bytes: &[u8],
    big: Option<bool>,
) -> Result<Vec<u32>, RuntimeError> {
    let (big, body) = resolve_utf32_bom(bytes, big);
    if body.len() % 4 != 0 {
        return Err(crate::error::unicode_decode_error(
            "utf-32",
            bytes,
            body.len() - (body.len() % 4),
            body.len(),
            "truncated data",
        ));
    }
    let mut out = Vec::with_capacity(body.len() / 4);
    for c in body.chunks_exact(4) {
        let v = if big {
            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
        } else {
            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
        };
        if v > 0x10_FFFF {
            return Err(crate::error::unicode_decode_error(
                "utf-32",
                bytes,
                0,
                4,
                "code point not in range(0x110000)",
            ));
        }
        out.push(v);
    }
    Ok(out)
}

/// Resolve a UTF-16 byte-order: when `big` is `None`, consume a leading BOM
/// (default little-endian if absent), returning the endianness and the body
/// after any BOM.
fn resolve_utf16_bom(bytes: &[u8], big: Option<bool>) -> (bool, &[u8]) {
    match big {
        Some(b) => (b, bytes),
        None => {
            if bytes.starts_with(&[0xFF, 0xFE]) {
                (false, &bytes[2..])
            } else if bytes.starts_with(&[0xFE, 0xFF]) {
                (true, &bytes[2..])
            } else {
                (false, bytes)
            }
        }
    }
}

/// Resolve a UTF-32 byte-order, consuming a leading BOM when `big` is `None`.
fn resolve_utf32_bom(bytes: &[u8], big: Option<bool>) -> (bool, &[u8]) {
    match big {
        Some(b) => (b, bytes),
        None => {
            if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
                (false, &bytes[4..])
            } else if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
                (true, &bytes[4..])
            } else {
                (false, bytes)
            }
        }
    }
}

pub fn decode_bytes(bytes: &[u8], encoding: &str, errors: &str) -> Result<String, RuntimeError> {
    check_error_handler(errors)?;
    if let Some(out) = decode_special(bytes, encoding, errors)? {
        return Ok(out);
    }
    if let Some(enc) = lookup_encoding(encoding) {
        let (text, _, had_errors) = enc.decode(bytes);
        if had_errors && errors == "strict" {
            return Err(value_error(format!(
                "'{encoding}' codec can't decode input"
            )));
        }
        return Ok(text.into_owned());
    }
    if let Some(out) = decode_via_registry(bytes, encoding, errors)? {
        return Ok(out);
    }
    Err(crate::error::lookup_error(format!(
        "unknown encoding: {encoding}"
    )))
}

// `REGISTRY_INFLIGHT`: encodings currently being resolved through the Python
// registry on this thread. Guards against a pathological codec whose
// `decode`/`encode` re-enters the native engine for the *same* encoding (which
// would loop); a re-entry returns `None` so the caller raises the normal
// `LookupError`.
thread_local! {
    static REGISTRY_INFLIGHT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Resolve `encoding` through the live `codecs` registry and run its stateless
/// `decode`. Returns `Ok(None)` when there is no interpreter, the encoding is
/// already in flight (recursion guard), `codecs.lookup` raised `LookupError`,
/// or the result isn't a `str` — in every such case the caller falls back to
/// its own `LookupError`.
fn decode_via_registry(
    bytes: &[u8],
    encoding: &str,
    errors: &str,
) -> Result<Option<String>, RuntimeError> {
    let Some(codec) = registry_codec_attr_text(encoding, "decode", "codecs.decode()")? else {
        return Ok(None);
    };
    let key = encoding.to_owned();
    REGISTRY_INFLIGHT.with(|s| s.borrow_mut().push(key.clone()));
    let res = with_interp(|interp| {
        interp.call_object(
            codec,
            &[Object::new_bytes(bytes.to_vec()), Object::from_str(errors)],
            &[],
        )
    });
    REGISTRY_INFLIGHT.with(|s| s.borrow_mut().retain(|e| e != &key));
    // CPython's `_PyCodec_DecodeInternal` notes every escaping exception
    // with the codec it came from (`wrap_codec_error`).
    let out = res.map_err(|e| add_codec_note(e, "decoding", encoding))?;
    let first = match &out {
        Object::Tuple(t) if !t.is_empty() => t[0].clone(),
        other => other.clone(),
    };
    match first {
        Object::Str(s) => Ok(Some(s.to_string())),
        // A codec was found and run, but returned a non-`str` result — an
        // unflagged binary-transform codec driven through the text model.
        // CPython's `_PyCodec_DecodeText` raises this exact `TypeError`.
        other => Err(type_error(format!(
            "'{encoding}' decoder returned '{}' instead of 'str'; \
             use codecs.decode() to decode to arbitrary types",
            other.type_name_owned()
        ))),
    }
}

/// Attach CPython's `wrap_codec_error` note ("encoding with 'X' codec
/// failed") to an escaping exception; non-exception errors pass through.
fn add_codec_note(err: RuntimeError, operation: &str, encoding: &str) -> RuntimeError {
    if let RuntimeError::PyException(pyexc) = &err {
        let note = format!("{operation} with '{encoding}' codec failed");
        let instance = pyexc.instance.clone();
        let _ = with_interp(|interp| {
            let add = interp.load_attr_public(&instance, "add_note")?;
            interp.call_object(add, &[Object::from_str(note.clone())], &[])
        });
    }
    err
}

/// `encode` counterpart to [`decode_via_registry`].
fn encode_via_registry(
    s: &str,
    encoding: &str,
    errors: &str,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    encode_via_registry_obj(Object::from_str(s), encoding, errors)
}

/// Object-taking variant of [`encode_via_registry`]: a surrogate-bearing
/// `WStr` is handed to the Python codec whole, so its own error-handler
/// machinery decides what happens to the lone surrogates (CPython gives the
/// codec the original str; e.g. the CJK codecs run custom handlers per
/// unencodable character rather than failing up front).
fn encode_via_registry_obj(
    s: Object,
    encoding: &str,
    errors: &str,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    let Some(codec) = registry_codec_attr_text(encoding, "encode", "codecs.encode()")? else {
        return Ok(None);
    };
    let key = encoding.to_owned();
    REGISTRY_INFLIGHT.with(|st| st.borrow_mut().push(key.clone()));
    let res = with_interp(|interp| interp.call_object(codec, &[s, Object::from_str(errors)], &[]));
    REGISTRY_INFLIGHT.with(|st| st.borrow_mut().retain(|e| e != &key));
    let out = res.map_err(|e| add_codec_note(e, "encoding", encoding))?;
    let first = match &out {
        Object::Tuple(t) if !t.is_empty() => t[0].clone(),
        other => other.clone(),
    };
    match first.as_bytes_view() {
        Some(b) => Ok(Some(b)),
        // Codec found and run, but returned a non-bytes result — an
        // unflagged binary-transform codec driven through the text model.
        // CPython's `_PyCodec_EncodeText` raises this exact `TypeError`.
        None => Err(type_error(format!(
            "'{encoding}' encoder returned '{}' instead of 'bytes'; \
             use codecs.encode() to encode to arbitrary types",
            first.type_name_owned()
        ))),
    }
}

/// Shared front half of the registry fallbacks: bail out (→ `Ok(None)`) when
/// there is no interpreter or the encoding is already being resolved, then
/// `codecs.lookup(encoding)` and return its `attr` (`"encode"`/`"decode"`)
/// callable. A `LookupError` from `lookup` is swallowed (→ `Ok(None)`).
/// Whether `codecs.lookup(encoding).decode` is `None` — i.e. the codec is
/// *incremental-only* (its sole decoder is `incrementaldecoder`, e.g.
/// test_io's `test_decoder`/`StatefulIncrementalDecoder`). Such a codec
/// cannot be driven by WeavePy's one-shot text fast path; the native
/// `TextIOWrapper` must fall back to the faithful incremental machinery
/// (`PyFile::read_text_incr`/`tell_text_incr`/`seek_text_incr`). A lookup
/// failure or a present one-shot decoder both report `false` (fast path).
pub fn codec_one_shot_decode_is_none(encoding: &str) -> bool {
    matches!(
        registry_codec_attr(encoding, "decode"),
        Ok(Some(Object::None))
    )
}

fn registry_codec_attr(encoding: &str, attr: &str) -> Result<Option<Object>, RuntimeError> {
    registry_codec_attr_inner(encoding, attr, None)
}

// The text-model `_is_text_encoding` rejection belongs at `TextIOWrapper`
// *construction* (CPython `_PyCodec_LookupTextEncoding`); the wrapper's own
// read/write operations must keep working even if the flag is flipped back
// afterwards (`test_io.test_illegal_decoder` constructs under a temporarily
// text-flagged quopri and expects `read()` to reach the codec and fail with
// TypeError, not LookupError). `PyFile` holds this scoped exemption around
// its per-operation codec calls.
thread_local! {
    static IO_TEXT_EXEMPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII guard suppressing the text-model check for the current thread.
#[derive(Debug)]
pub struct IoTextExemptGuard(bool);

pub fn io_text_exempt_guard() -> IoTextExemptGuard {
    let prev = IO_TEXT_EXEMPT.with(|c| c.replace(true));
    IoTextExemptGuard(prev)
}

impl Drop for IoTextExemptGuard {
    fn drop(&mut self) {
        let prev = self.0;
        IO_TEXT_EXEMPT.with(|c| c.set(prev));
    }
}

/// [`registry_codec_attr`] for the *text model* (`str.encode` /
/// `bytes.decode`): a codec flagged `_is_text_encoding=False` raises
/// CPython's `_PyCodec_LookupTextEncoding` `LookupError` with the
/// operation-appropriate `alternate_command` hint.
fn registry_codec_attr_text(
    encoding: &str,
    attr: &str,
    alternate_command: &str,
) -> Result<Option<Object>, RuntimeError> {
    registry_codec_attr_inner(encoding, attr, Some(alternate_command))
}

fn registry_codec_attr_inner(
    encoding: &str,
    attr: &str,
    text_only_hint: Option<&str>,
) -> Result<Option<Object>, RuntimeError> {
    if crate::vm_singletons::current_interpreter_ptr().is_none() {
        return Ok(None);
    }
    let reentrant = REGISTRY_INFLIGHT.with(|s| s.borrow().iter().any(|e| e == encoding));
    if reentrant {
        return Ok(None);
    }
    with_interp(|interp| {
        let Ok(codecs) = interp.import_path("codecs") else {
            return Ok(None);
        };
        let Ok(lookup) = interp.load_attr_public(&codecs, "lookup") else {
            return Ok(None);
        };
        let info = match interp.call_object(lookup, &[Object::from_str(encoding)], &[]) {
            Ok(i) => i,
            Err(_) => return Ok(None),
        };
        if let Some(hint) = text_only_hint {
            if !IO_TEXT_EXEMPT.with(|c| c.get()) {
                let is_text = interp
                    .load_attr_public(&info, "_is_text_encoding")
                    .map(|o| o.is_truthy())
                    .unwrap_or(true);
                if !is_text {
                    return Err(crate::error::lookup_error(format!(
                        "'{encoding}' is not a text encoding; use {hint} to handle arbitrary codecs"
                    )));
                }
            }
        }
        match interp.load_attr_public(&info, attr) {
            Ok(c) => Ok(Some(c)),
            Err(_) => Ok(None),
        }
    })
}

/// The canonical CPython codec name for `encoding`
/// (`codecs.lookup(...).name`): `latin1` → `iso8859-1`. Used at startup to
/// report `sys.std*.encoding` normalized, matching `config_init_stdio`.
/// `None` when the registry can't resolve the name (unknown codec, or no
/// interpreter published on this thread).
pub fn canonical_codec_name(encoding: &str) -> Option<String> {
    match registry_codec_attr_inner(encoding, "name", None) {
        Ok(Some(Object::Str(s))) => Some(s.to_string()),
        _ => None,
    }
}

/// Run `f` with the current interpreter. The pointer is published by an
/// enclosing VM frame on this thread and the GIL keeps the reentrant access
/// exclusive (same contract as `io_full::validate_text_encoding`).
fn with_interp<T>(
    f: impl FnOnce(&mut crate::Interpreter) -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: see doc comment.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

/// Emit a `DeprecationWarning` from the unicode-escape codec (CPython's
/// `_PyUnicode_DecodeUnicodeEscapeStateful` warns about the first invalid
/// escape sequence). An escalating warnings filter turns this into an error.
fn emit_deprecation(msg: &str) -> Result<(), RuntimeError> {
    with_interp(|interp| interp.warn_deprecation_from_builtin(msg.to_owned()))
}

/// Handle special-case encodings whose semantics don't quite match
/// `encoding_rs`'s default behaviour (utf-8 with `surrogateescape`,
/// latin-1, raw_unicode_escape, etc.).
fn encode_special(s: &str, encoding: &str, errors: &str) -> Result<Option<Vec<u8>>, RuntimeError> {
    let key = encoding_key(encoding);
    // The UTF/ASCII/Latin-1 family routes through the unified engine so
    // *custom* error handlers (`codecs.register_error`) work from
    // `str.encode` too, not just the `_codecs` entry points (RFC 0050 WS2).
    let cps = || -> Vec<u32> { s.chars().map(|c| c as u32).collect() };
    Ok(match key.as_str() {
        "utf8" => Some(crate::stdlib::codecs_engine::utf8_encode(&cps(), errors)?),
        "utf8sig" => {
            // UTF-8 with a leading BOM (CPython `utf_8_sig`). The stateless
            // codec always prepends the BOM; the BOM-once-per-stream nuance
            // lives in the incremental encoder (frozen `codecs.py`).
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend(crate::stdlib::codecs_engine::utf8_encode(&cps(), errors)?);
            Some(out)
        }
        "ascii" => Some(crate::stdlib::codecs_engine::ascii_encode(&cps(), errors)?),
        "latin1" | "iso88591" => Some(crate::stdlib::codecs_engine::latin1_encode(&cps(), errors)?),
        "utf16" => Some(crate::stdlib::codecs_engine::utf16_encode(
            &cps(),
            errors,
            0,
        )?),
        "utf16le" => Some(crate::stdlib::codecs_engine::utf16_encode(
            &cps(),
            errors,
            -1,
        )?),
        "utf16be" => Some(crate::stdlib::codecs_engine::utf16_encode(
            &cps(),
            errors,
            1,
        )?),
        "utf32" => Some(crate::stdlib::codecs_engine::utf32_encode(
            &cps(),
            errors,
            0,
        )?),
        "utf32le" => Some(crate::stdlib::codecs_engine::utf32_encode(
            &cps(),
            errors,
            -1,
        )?),
        "utf32be" => Some(crate::stdlib::codecs_engine::utf32_encode(
            &cps(),
            errors,
            1,
        )?),
        "rawunicodeescape" => Some(encode_raw_unicode_escape(s)),
        "unicodeescape" => Some(encode_unicode_escape(s)),
        "utf7" => Some(crate::stdlib::codecs_engine::utf7_encode(&cps(), errors)?),
        // Any single-byte codec (iso-8859-*, cp*, koi8-*, mac-*): the
        // charmap engine, so custom error handlers work uniformly.
        _ => match sbcs_decode_table(encoding) {
            Some(table) => Some(crate::stdlib::codecs_engine::charmap_encode_table(
                &cps(),
                errors,
                table,
            )?),
            None => None,
        },
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
        "utf8sig" => {
            // Strip a single leading UTF-8 BOM if present, then decode.
            let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
            Some(decode_utf8(body, errors)?)
        }
        "ascii" => Some(decode_ascii(bytes, errors)?),
        "latin1" | "iso88591" => Some(decode_latin1(bytes)),
        "utf16" => Some(decode_utf16(bytes, None)?),
        "utf16le" => Some(decode_utf16(bytes, Some(false))?),
        "utf16be" => Some(decode_utf16(bytes, Some(true))?),
        "utf32" => Some(decode_utf32(bytes, None)?),
        "utf32le" => Some(decode_utf32(bytes, Some(false))?),
        "utf32be" => Some(decode_utf32(bytes, Some(true))?),
        "rawunicodeescape" => Some(
            crate::stdlib::codecs_engine::raw_unicode_escape_decode(bytes, errors, true)?
                .0
                .to_str(),
        ),
        "unicodeescape" => {
            let (obj, _, warn) =
                crate::stdlib::codecs_engine::unicode_escape_decode(bytes, errors, true)?;
            if let Some(msg) = warn {
                emit_deprecation(&msg)?;
            }
            Some(obj.to_str())
        }
        "utf7" => Some(decode_utf7(bytes, errors)?),
        _ => match sbcs_decode_table(encoding) {
            Some(table) => Some(
                crate::stdlib::codecs_engine::charmap_decode_table(bytes, errors, table)?
                    .0
                    .to_str(),
            ),
            None => None,
        },
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

// ---------- cp1252 (Windows-1252, strict charmap) ----------
//
// `encoding_rs` implements the *WHATWG* windows-1252 index, which fills the five
// positions CPython leaves undefined (0x81, 0x8D, 0x8F, 0x90, 0x9D) with the C1
// control code points — so `'\x9d'.encode('cp1252')` there silently round-trips
// instead of raising. CPython's `Lib/encodings/cp1252.py` is a strict charmap
// with those five slots unmapped; reproduce it exactly (both directions, with
// the built-in error handlers) so `Series.str.encode/decode('cp1252', errors=…)`
// matches. The codec name in the raised error is `'charmap'`, like CPython.

/// Upper half (0x80..=0xFF) of Windows code page 1252, from CPython's
/// `Lib/encodings/cp1252.py` decoding table. `None` marks the five
/// positions (0x81, 0x8D, 0x8F, 0x90, 0x9D) that CPython leaves undefined
/// (WHATWG windows-1252, used by `encoding_rs`, wrongly maps them to the
/// C1 controls, so `'\x9d'.encode('cp1252')` must not silently round-trip).
const CP1252_HIGH: [Option<char>; 128] = [
    Some('\u{20AC}'),
    None,
    Some('\u{201A}'),
    Some('\u{0192}'),
    Some('\u{201E}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02C6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017D}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02DC}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203A}'),
    Some('\u{0153}'),
    None,
    Some('\u{017E}'),
    Some('\u{0178}'),
    Some('\u{00A0}'),
    Some('\u{00A1}'),
    Some('\u{00A2}'),
    Some('\u{00A3}'),
    Some('\u{00A4}'),
    Some('\u{00A5}'),
    Some('\u{00A6}'),
    Some('\u{00A7}'),
    Some('\u{00A8}'),
    Some('\u{00A9}'),
    Some('\u{00AA}'),
    Some('\u{00AB}'),
    Some('\u{00AC}'),
    Some('\u{00AD}'),
    Some('\u{00AE}'),
    Some('\u{00AF}'),
    Some('\u{00B0}'),
    Some('\u{00B1}'),
    Some('\u{00B2}'),
    Some('\u{00B3}'),
    Some('\u{00B4}'),
    Some('\u{00B5}'),
    Some('\u{00B6}'),
    Some('\u{00B7}'),
    Some('\u{00B8}'),
    Some('\u{00B9}'),
    Some('\u{00BA}'),
    Some('\u{00BB}'),
    Some('\u{00BC}'),
    Some('\u{00BD}'),
    Some('\u{00BE}'),
    Some('\u{00BF}'),
    Some('\u{00C0}'),
    Some('\u{00C1}'),
    Some('\u{00C2}'),
    Some('\u{00C3}'),
    Some('\u{00C4}'),
    Some('\u{00C5}'),
    Some('\u{00C6}'),
    Some('\u{00C7}'),
    Some('\u{00C8}'),
    Some('\u{00C9}'),
    Some('\u{00CA}'),
    Some('\u{00CB}'),
    Some('\u{00CC}'),
    Some('\u{00CD}'),
    Some('\u{00CE}'),
    Some('\u{00CF}'),
    Some('\u{00D0}'),
    Some('\u{00D1}'),
    Some('\u{00D2}'),
    Some('\u{00D3}'),
    Some('\u{00D4}'),
    Some('\u{00D5}'),
    Some('\u{00D6}'),
    Some('\u{00D7}'),
    Some('\u{00D8}'),
    Some('\u{00D9}'),
    Some('\u{00DA}'),
    Some('\u{00DB}'),
    Some('\u{00DC}'),
    Some('\u{00DD}'),
    Some('\u{00DE}'),
    Some('\u{00DF}'),
    Some('\u{00E0}'),
    Some('\u{00E1}'),
    Some('\u{00E2}'),
    Some('\u{00E3}'),
    Some('\u{00E4}'),
    Some('\u{00E5}'),
    Some('\u{00E6}'),
    Some('\u{00E7}'),
    Some('\u{00E8}'),
    Some('\u{00E9}'),
    Some('\u{00EA}'),
    Some('\u{00EB}'),
    Some('\u{00EC}'),
    Some('\u{00ED}'),
    Some('\u{00EE}'),
    Some('\u{00EF}'),
    Some('\u{00F0}'),
    Some('\u{00F1}'),
    Some('\u{00F2}'),
    Some('\u{00F3}'),
    Some('\u{00F4}'),
    Some('\u{00F5}'),
    Some('\u{00F6}'),
    Some('\u{00F7}'),
    Some('\u{00F8}'),
    Some('\u{00F9}'),
    Some('\u{00FA}'),
    Some('\u{00FB}'),
    Some('\u{00FC}'),
    Some('\u{00FD}'),
    Some('\u{00FE}'),
    Some('\u{00FF}'),
];

// ---------- UTF-7 (RFC 2152) ----------
//
// `encoding_rs` has no UTF-7, but real code drives it (e.g. `tarfile` opened
// with `encoding='utf7'`). Ported faithfully from CPython 3.13's
// `_PyUnicode_EncodeUTF7` / `PyUnicode_DecodeUTF7Stateful`
// (Objects/unicodeobject.c). The stateless codec encodes the modified-Base64
// shifted sequences over UTF-16 code units. Because WeavePy's `str` is strict
// UTF-8, lone surrogates produced by malformed input become U+FFFD (the same
// concession the UTF-8 surrogateescape path makes).

#[inline]
fn utf7_is_base64(c: u32) -> bool {
    if c > 127 {
        return false;
    }
    let b = c as u8;
    b.is_ascii_uppercase() || b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'+' || b == b'/'
}

#[inline]
fn utf7_from_base64(c: u32) -> u64 {
    let b = c as u8;
    if b.is_ascii_uppercase() {
        u64::from(b - b'A')
    } else if b.is_ascii_lowercase() {
        u64::from(b - b'a') + 26
    } else if b.is_ascii_digit() {
        u64::from(b - b'0') + 52
    } else if b == b'+' {
        62
    } else {
        63
    }
}

/// `DECODE_DIRECT`: an ASCII byte (other than `+`) that decodes as itself.
#[inline]
fn utf7_decode_direct(c: u32) -> bool {
    c <= 127 && c != u32::from(b'+')
}

/// Push a (possibly surrogate) code point; WeavePy `str` can't hold lone
/// surrogates, so they degrade to U+FFFD (see module note).
#[inline]
fn utf7_push(out: &mut String, cp: u32) {
    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
}

fn decode_utf7(bytes: &[u8], errors: &str) -> Result<String, RuntimeError> {
    let mut out = String::with_capacity(bytes.len());
    let mut in_shift = false;
    let mut base64bits: u32 = 0;
    let mut base64buffer: u64 = 0;
    let mut surrogate: u32 = 0;
    let e = bytes.len();
    let mut s = 0usize;

    // Apply the configured error handler to a decode error spanning
    // `start..end`. Returns `Err` for strict; otherwise substitutes
    // (`replace`) or drops (`ignore`/others) and lets scanning continue.
    macro_rules! utf7_error {
        ($start:expr, $end:expr, $reason:expr) => {{
            match errors {
                "ignore" => {}
                _ => {
                    if errors == "strict" {
                        return Err(crate::error::unicode_decode_error(
                            "utf7", bytes, $start, $end, $reason,
                        ));
                    }
                    // `replace`, `backslashreplace`, etc. — best-effort U+FFFD.
                    out.push('\u{FFFD}');
                }
            }
        }};
    }

    while s < e {
        let ch = u32::from(bytes[s]);
        if in_shift {
            if utf7_is_base64(ch) {
                base64buffer = (base64buffer << 6) | utf7_from_base64(ch);
                base64bits += 6;
                s += 1;
                if base64bits >= 16 {
                    let out_ch = ((base64buffer >> (base64bits - 16)) & 0xFFFF) as u32;
                    base64bits -= 16;
                    base64buffer &= (1u64 << base64bits) - 1;
                    if surrogate != 0 {
                        if (0xDC00..=0xDFFF).contains(&out_ch) {
                            let joined = 0x10000 + ((surrogate - 0xD800) << 10) + (out_ch - 0xDC00);
                            utf7_push(&mut out, joined);
                            surrogate = 0;
                            continue;
                        }
                        utf7_push(&mut out, surrogate);
                        surrogate = 0;
                    }
                    if (0xD800..=0xDBFF).contains(&out_ch) {
                        surrogate = out_ch;
                    } else {
                        utf7_push(&mut out, out_ch);
                    }
                }
            } else {
                // Leaving a base-64 section.
                in_shift = false;
                if base64bits >= 6 {
                    let start = s;
                    s += 1;
                    utf7_error!(start, s, "partial character in shift sequence");
                    base64bits = 0;
                    base64buffer = 0;
                    surrogate = 0;
                    continue;
                } else if base64bits > 0 && base64buffer != 0 {
                    let start = s;
                    s += 1;
                    utf7_error!(start, s, "non-zero padding bits in shift sequence");
                    base64bits = 0;
                    base64buffer = 0;
                    surrogate = 0;
                    continue;
                }
                if surrogate != 0 && utf7_decode_direct(ch) {
                    utf7_push(&mut out, surrogate);
                }
                surrogate = 0;
                base64bits = 0;
                base64buffer = 0;
                if ch == u32::from(b'-') {
                    s += 1;
                }
            }
        } else if ch == u32::from(b'+') {
            let start = s;
            s += 1;
            if s < e && bytes[s] == b'-' {
                s += 1;
                out.push('+');
            } else if s < e && !utf7_is_base64(u32::from(bytes[s])) {
                s += 1;
                utf7_error!(start, s, "ill-formed sequence");
            } else {
                in_shift = true;
                surrogate = 0;
                base64bits = 0;
                base64buffer = 0;
            }
        } else if utf7_decode_direct(ch) {
            s += 1;
            out.push(ch as u8 as char);
        } else {
            let start = s;
            s += 1;
            utf7_error!(start, s, "unexpected special character");
        }
    }

    if in_shift && (surrogate != 0 || base64bits >= 6 || (base64bits > 0 && base64buffer != 0)) {
        utf7_error!(s, e, "unterminated shift sequence");
    }
    Ok(out)
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
    let normalised: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-' && *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    // Canonicalise the common `Lib/encodings/aliases.py` spellings onto the keys
    // the native fast paths (`encode_special`/`decode_special`/
    // `encode_codepoints`) switch on. This matters for correctness, not just
    // dispatch: without it `us-ascii`/`cp819`/… miss the strict native codecs and
    // fall through to `lookup_encoding`, where `encoding_rs` resolves the
    // `us-ascii` and `iso-8859-1` *labels* to the lenient WHATWG windows-1252
    // superset — so e.g. `'\xeb'.encode('us-ascii')` would wrongly succeed
    // instead of raising `UnicodeEncodeError` (breaks RFC 2047 header folding).
    match normalised.as_str() {
        "usascii" | "iso646us" | "646" | "cp367" | "ibm367" | "csascii" | "us" => {
            "ascii".to_owned()
        }
        "latin" | "cp819" | "l1" | "8859" | "csisolatin1" | "ibm819" | "isoir100"
        | "iso885911987" => "latin1".to_owned(),
        _ => normalised,
    }
}

/// Codecs whose encoded byte stream can embed the newline bytes `0x0A`/`0x0D`
/// *inside* a multi-byte code unit (or encode a newline as several bytes),
/// making raw byte-level newline scanning invalid. These are exactly the
/// UTF-16 and UTF-32 families: a byte-backed text stream opened with one of
/// them must find line boundaries in the *decoded* text via the incremental
/// decoder, precisely like CPython's `TextIOWrapper`. (UTF-8, UTF-8-sig and
/// UTF-7 are newline-*safe* — the newline bytes never appear as a
/// continuation byte — so they keep the fast byte-scanning read path.)
pub fn codec_is_newline_unsafe(encoding: &str) -> bool {
    matches!(
        encoding_key(encoding).as_str(),
        "utf16"
            | "utf16le"
            | "utf16be"
            | "u16"
            | "unicodebigunmarked"
            | "unicodelittleunmarked"
            | "utf32"
            | "utf32le"
            | "utf32be"
            | "u32"
    )
}

// ---------- UTF-8 ----------

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

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
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
        // CPython emits code points < 0x100 as raw latin-1 bytes; only
        // higher planes get `\u`/`\U` escapes.
        if cp < 0x100 {
            out.push(cp as u8);
        } else if cp <= 0xFFFF {
            out.extend_from_slice(format!("\\u{:04x}", cp).as_bytes());
        } else {
            out.extend_from_slice(format!("\\U{:08x}", cp).as_bytes());
        }
    }
    out
}

fn encode_unicode_escape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            // Quotes stay literal — escaping them is `repr`'s job, not the
            // codec's (CPython `PyUnicode_AsUnicodeEscapeString`).
            ch if (ch as u32) < 0x20 || ch == '\x7f' => {
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

enc_encoder!(b_cp1252_encode, "cp1252");
enc_decoder!(b_cp1252_decode, "cp1252");

// ---------- engine-backed entry points (RFC 0050 WS2) ----------
//
// These expose CPython's exact `_codecs` signatures: decoders speak the
// stateful protocol (`final=False` leaves a trailing incomplete sequence
// unconsumed), the `_ex` variants return `(text, consumed, byteorder)`,
// and every coder resolves error handlers through the unified machinery
// in `codecs_engine` (built-ins natively, custom handlers via the live
// `codecs.register_error` registry).

use crate::stdlib::codecs_engine as engine;

/// Truthiness of the optional `final` flag argument.
fn arg_final(args: &[Object], idx: usize) -> bool {
    args.get(idx).is_some_and(|o| o.is_truthy())
}

/// Like [`arg_final`], but defaulting to `true` when absent (the escape
/// codecs' `final=True` default).
fn arg_final_default_true(args: &[Object], idx: usize) -> bool {
    args.get(idx).is_none_or(|o| o.is_truthy())
}

/// The optional `byteorder` int argument (0 = sniff BOM / native).
fn arg_byteorder(args: &[Object], idx: usize) -> i32 {
    match args.get(idx) {
        Some(Object::Int(i)) => (*i).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        _ => 0,
    }
}

/// The text argument as raw code points (`Str` fast path, `WStr` raw).
fn arg_text_codepoints(args: &[Object], idx: usize, name: &str) -> Result<Vec<u32>, RuntimeError> {
    match args.get(idx) {
        Some(o) => engine::str_codepoints(o).ok_or_else(|| {
            type_error(format!(
                "{name}() argument 'str' must be str, not {}",
                o.type_name()
            ))
        }),
        None => Err(type_error(format!("{name}() missing required argument"))),
    }
}

fn enc_tuple(bytes: Vec<u8>, nchars: usize) -> Object {
    Object::new_tuple(vec![Object::new_bytes(bytes), Object::Int(nchars as i64)])
}

fn dec_tuple(text: Object, consumed: usize) -> Object {
    Object::new_tuple(vec![text, Object::Int(consumed as i64)])
}

fn b_utf8_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let cps = arg_text_codepoints(args, 0, "utf_8_encode")?;
    let errors = arg_errors(args, 1);
    Ok(enc_tuple(engine::utf8_encode(&cps, &errors)?, cps.len()))
}

fn b_utf8_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "utf_8_decode")?;
    let errors = arg_errors(args, 1);
    let (text, consumed) = engine::utf8_decode(&bytes, &errors, arg_final(args, 2))?;
    Ok(dec_tuple(text, consumed))
}

fn b_utf7_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let cps = arg_text_codepoints(args, 0, "utf_7_encode")?;
    let errors = arg_errors(args, 1);
    Ok(enc_tuple(engine::utf7_encode(&cps, &errors)?, cps.len()))
}

fn b_utf7_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "utf_7_decode")?;
    let errors = arg_errors(args, 1);
    let (text, consumed) = engine::utf7_decode(&bytes, &errors, arg_final(args, 2))?;
    Ok(dec_tuple(text, consumed))
}

fn b_ascii_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let cps = arg_text_codepoints(args, 0, "ascii_encode")?;
    let errors = arg_errors(args, 1);
    Ok(enc_tuple(engine::ascii_encode(&cps, &errors)?, cps.len()))
}

fn b_ascii_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "ascii_decode")?;
    let errors = arg_errors(args, 1);
    let (text, consumed) = engine::ascii_decode(&bytes, &errors)?;
    Ok(dec_tuple(text, consumed))
}

fn b_latin1_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let cps = arg_text_codepoints(args, 0, "latin_1_encode")?;
    let errors = arg_errors(args, 1);
    Ok(enc_tuple(engine::latin1_encode(&cps, &errors)?, cps.len()))
}

fn b_latin1_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes(args, 0, "latin_1_decode")?;
    let errors = arg_errors(args, 1);
    let (text, consumed) = engine::latin1_decode(&bytes, &errors)?;
    Ok(dec_tuple(text, consumed))
}

fn b_readbuffer_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    // Any buffer (str encodes as UTF-8) copied out verbatim.
    let bytes = match args.first() {
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        Some(o) => match o.as_bytes_view() {
            Some(b) => b,
            // Other buffer-protocol objects (`array.array`): copy out via
            // their `tobytes()`.
            None => with_interp(|interp| {
                let m = interp.load_attr_public(o, "tobytes")?;
                interp.call_object(m, &[], &[])
            })
            .ok()
            .and_then(|r| r.as_bytes_view())
            .ok_or_else(|| {
                type_error(format!(
                    "readbuffer_encode() argument 'data' must be read-only bytes-like object, not {}",
                    o.type_name_owned()
                ))
            })?,
        },
        None => return Err(type_error("readbuffer_encode() missing required argument")),
    };
    let len = bytes.len();
    Ok(enc_tuple(bytes, len))
}

macro_rules! utf1632_encode {
    ($name:ident, $engine:path, $pyname:literal, $byteorder:expr) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            let cps = arg_text_codepoints(args, 0, $pyname)?;
            let errors = arg_errors(args, 1);
            // Byte-order-less variants take an optional byteorder argument.
            let bo: i32 = match $byteorder {
                Some(fixed) => fixed,
                None => arg_byteorder(args, 2),
            };
            Ok(enc_tuple($engine(&cps, &errors, bo)?, cps.len()))
        }
    };
}

macro_rules! utf1632_decode {
    ($name:ident, $engine:path, $pyname:literal, $byteorder:expr, ex $ex:literal) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            let bytes = arg_bytes(args, 0, $pyname)?;
            let errors = arg_errors(args, 1);
            let (bo, final_) = match $byteorder {
                Some(fixed) => (fixed, arg_final(args, 2)),
                None if $ex => (arg_byteorder(args, 2), arg_final(args, 3)),
                None => (0, arg_final(args, 2)),
            };
            let (text, consumed, out_bo) = $engine(&bytes, &errors, bo, final_)?;
            if $ex {
                Ok(Object::new_tuple(vec![
                    text,
                    Object::Int(consumed as i64),
                    Object::Int(i64::from(out_bo)),
                ]))
            } else {
                Ok(dec_tuple(text, consumed))
            }
        }
    };
}

utf1632_encode!(
    b_utf16_encode,
    engine::utf16_encode,
    "utf_16_encode",
    None::<i32>
);
utf1632_encode!(
    b_utf16_le_encode,
    engine::utf16_encode,
    "utf_16_le_encode",
    Some(-1)
);
utf1632_encode!(
    b_utf16_be_encode,
    engine::utf16_encode,
    "utf_16_be_encode",
    Some(1)
);
utf1632_encode!(
    b_utf32_encode,
    engine::utf32_encode,
    "utf_32_encode",
    None::<i32>
);
utf1632_encode!(
    b_utf32_le_encode,
    engine::utf32_encode,
    "utf_32_le_encode",
    Some(-1)
);
utf1632_encode!(
    b_utf32_be_encode,
    engine::utf32_encode,
    "utf_32_be_encode",
    Some(1)
);

utf1632_decode!(b_utf16_decode, engine::utf16_decode, "utf_16_decode", None::<i32>, ex false);
utf1632_decode!(b_utf16_le_decode, engine::utf16_decode, "utf_16_le_decode", Some(-1), ex false);
utf1632_decode!(b_utf16_be_decode, engine::utf16_decode, "utf_16_be_decode", Some(1), ex false);
utf1632_decode!(b_utf16_ex_decode, engine::utf16_decode, "utf_16_ex_decode", None::<i32>, ex true);
utf1632_decode!(b_utf32_decode, engine::utf32_decode, "utf_32_decode", None::<i32>, ex false);
utf1632_decode!(b_utf32_le_decode, engine::utf32_decode, "utf_32_le_decode", Some(-1), ex false);
utf1632_decode!(b_utf32_be_decode, engine::utf32_decode, "utf_32_be_decode", Some(1), ex false);
utf1632_decode!(b_utf32_ex_decode, engine::utf32_decode, "utf_32_ex_decode", None::<i32>, ex true);
enc_encoder!(b_raw_unicode_escape_encode, "raw_unicode_escape");
enc_encoder!(b_unicode_escape_encode, "unicode_escape");

/// `_codecs.raw_unicode_escape_decode(data, errors="strict", final=True)`.
fn b_raw_unicode_escape_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes_or_str_utf8(args, 0, "raw_unicode_escape_decode")?;
    let errors = arg_errors(args, 1);
    let final_ = arg_final_default_true(args, 2);
    let (obj, consumed) =
        crate::stdlib::codecs_engine::raw_unicode_escape_decode(&bytes, &errors, final_)?;
    Ok(Object::new_tuple(vec![obj, Object::Int(consumed as i64)]))
}

/// `_codecs.unicode_escape_decode(data, errors="strict", final=True)`.
/// Emits CPython's first-invalid-escape `DeprecationWarning`.
fn b_unicode_escape_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = arg_bytes_or_str_utf8(args, 0, "unicode_escape_decode")?;
    let errors = arg_errors(args, 1);
    let final_ = arg_final_default_true(args, 2);
    let (obj, consumed, warn) =
        crate::stdlib::codecs_engine::unicode_escape_decode(&bytes, &errors, final_)?;
    if let Some(msg) = warn {
        emit_deprecation(&msg)?;
    }
    Ok(Object::new_tuple(vec![obj, Object::Int(consumed as i64)]))
}

// ---------- Windows code-page codecs (RFC 0063 WS6) ----------
//
// CPython-on-Windows exposes `_codecs.code_page_encode`/`code_page_decode`
// plus the `mbcs_*` (CP_ACP) and `oem_*` (CP_OEMCP) wrappers; the frozen
// `encodings/mbcs.py` and `encodings/oem.py` modules build their codecs on
// top of these. The conversion engine is `MultiByteToWideChar` /
// `WideCharToMultiByte`, with CPython's two-phase strategy: a strict
// whole-buffer pass first, then a per-character pass that runs the error
// handler (`Objects/unicodeobject.c` `decode_code_page_errors` /
// `encode_code_page_errors`).
#[cfg(windows)]
mod nt_code_page {
    use super::{
        arg_bytes, arg_errors, arg_final, arg_text_codepoints, dec_tuple, enc_tuple, type_error,
        value_error, Object, RuntimeError,
    };
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_INVALID_FLAGS, ERROR_NO_UNICODE_TRANSLATION,
    };
    use windows_sys::Win32::Globalization::{
        GetOEMCP, IsDBCSLeadByteEx, MultiByteToWideChar, WideCharToMultiByte, CP_OEMCP, CP_UTF7,
        CP_UTF8, MB_ERR_INVALID_CHARS, WC_NO_BEST_FIT_CHARS,
    };

    const CP_ACP: u32 = 0;
    const DECODE_REASON: &str =
        "No mapping for the Unicode character exists in the target code page.";
    const ENCODE_REASON: &str = "invalid character";

    /// CPython `code_page_name`: `CP_ACP` reports as `"mbcs"`, `CP_OEMCP`
    /// resolves to the concrete OEM code page, everything else is `cp%u`.
    fn code_page_name(cp: u32) -> String {
        if cp == CP_ACP {
            return "mbcs".to_owned();
        }
        let cp = if cp == CP_OEMCP {
            unsafe { GetOEMCP() }
        } else {
            cp
        };
        format!("cp{cp}")
    }

    /// UTF-16 units → code points (surrogate pairs combined, lone
    /// surrogates preserved for the `WStr` path).
    fn utf16_to_cps(w: &[u16]) -> Vec<u32> {
        let mut out = Vec::with_capacity(w.len());
        let mut i = 0;
        while i < w.len() {
            let u = w[i];
            if (0xD800..0xDC00).contains(&u)
                && i + 1 < w.len()
                && (0xDC00..0xE000).contains(&w[i + 1])
            {
                let cp = 0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(w[i + 1]) - 0xDC00);
                out.push(cp);
                i += 2;
            } else {
                out.push(u32::from(u));
                i += 1;
            }
        }
        out
    }

    /// Code points → UTF-16 units (lone surrogates pass through so the
    /// per-character error path sees them and can raise/escape).
    fn cps_to_utf16(cps: &[u32]) -> Vec<u16> {
        let mut out = Vec::with_capacity(cps.len());
        for &cp in cps {
            if cp >= 0x10000 {
                let v = cp - 0x10000;
                out.push(0xD800 + (v >> 10) as u16);
                out.push(0xDC00 + (v & 0x3FF) as u16);
            } else {
                out.push(cp as u16);
            }
        }
        out
    }

    fn mb_to_wc(cp: u32, flags: u32, bytes: &[u8]) -> Result<Vec<u16>, u32> {
        debug_assert!(!bytes.is_empty());
        unsafe {
            let n = MultiByteToWideChar(
                cp,
                flags,
                bytes.as_ptr(),
                bytes.len() as i32,
                std::ptr::null_mut(),
                0,
            );
            if n <= 0 {
                return Err(GetLastError());
            }
            let mut buf = vec![0u16; n as usize];
            let n2 = MultiByteToWideChar(
                cp,
                flags,
                bytes.as_ptr(),
                bytes.len() as i32,
                buf.as_mut_ptr(),
                n,
            );
            if n2 <= 0 {
                return Err(GetLastError());
            }
            buf.truncate(n2 as usize);
            Ok(buf)
        }
    }

    /// One `WideCharToMultiByte` round trip; `used_default` reports whether
    /// the system substituted the code page's default character (CPython's
    /// "-2 → run the error handler" signal).
    fn wc_to_mb(cp: u32, wide: &[u16], used_default: &mut bool) -> Result<Vec<u8>, u32> {
        debug_assert!(!wide.is_empty());
        // CP_UTF7/CP_UTF8 reject WC_NO_BEST_FIT_CHARS and the default-char
        // out-params (ERROR_INVALID_FLAGS); CPython special-cases them the
        // same way.
        let plain = cp == CP_UTF7 || cp == CP_UTF8;
        unsafe {
            let flags = if plain { 0 } else { WC_NO_BEST_FIT_CHARS };
            // `BOOL` out-param; windows-sys defines it as `i32`.
            let mut used: i32 = 0;
            let pused: *mut i32 = if plain {
                std::ptr::null_mut()
            } else {
                &raw mut used
            };
            let n = WideCharToMultiByte(
                cp,
                flags,
                wide.as_ptr(),
                wide.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            if n <= 0 {
                let err = GetLastError();
                if err == ERROR_INVALID_FLAGS && !plain {
                    // Code page without WC_NO_BEST_FIT_CHARS support: retry
                    // flagless, like CPython's encode_code_page_strict.
                    return wc_to_mb_flagless(cp, wide);
                }
                return Err(err);
            }
            let mut buf = vec![0u8; n as usize];
            let n2 = WideCharToMultiByte(
                cp,
                flags,
                wide.as_ptr(),
                wide.len() as i32,
                buf.as_mut_ptr(),
                n,
                std::ptr::null(),
                pused,
            );
            if n2 <= 0 {
                return Err(GetLastError());
            }
            buf.truncate(n2 as usize);
            *used_default = used != 0;
            Ok(buf)
        }
    }

    fn wc_to_mb_flagless(cp: u32, wide: &[u16]) -> Result<Vec<u8>, u32> {
        unsafe {
            let n = WideCharToMultiByte(
                cp,
                0,
                wide.as_ptr(),
                wide.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            if n <= 0 {
                return Err(GetLastError());
            }
            let mut buf = vec![0u8; n as usize];
            let n2 = WideCharToMultiByte(
                cp,
                0,
                wide.as_ptr(),
                wide.len() as i32,
                buf.as_mut_ptr(),
                n,
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            if n2 <= 0 {
                return Err(GetLastError());
            }
            buf.truncate(n2 as usize);
            Ok(buf)
        }
    }

    /// CPython `decode_code_page_errors`: walk the input DBCS-sequence by
    /// DBCS-sequence, running the error handler on undecodable bytes.
    /// Returns `(code points, bytes consumed)` — with `final=False` an
    /// incomplete trailing sequence is left unconsumed.
    fn decode_errors(
        cp: u32,
        bytes: &[u8],
        errors: &str,
        final_: bool,
    ) -> Result<(Vec<u32>, usize), RuntimeError> {
        let name = code_page_name(cp);
        let mut out: Vec<u32> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let insize = if unsafe { IsDBCSLeadByteEx(cp, bytes[i]) } != 0 {
                2
            } else {
                1
            };
            if i + insize > bytes.len() {
                // Truncated multibyte sequence at the end of the input.
                if !final_ {
                    break;
                }
                match errors {
                    "strict" => {
                        return Err(crate::error::unicode_decode_error(
                            &name,
                            bytes,
                            i,
                            bytes.len(),
                            DECODE_REASON,
                        ));
                    }
                    "ignore" => {}
                    "replace" => out.push(0xFFFD),
                    "surrogateescape" => {
                        for &b in &bytes[i..] {
                            out.push(0xDC00 + u32::from(b));
                        }
                    }
                    "backslashreplace" => {
                        for &b in &bytes[i..] {
                            for c in format!("\\x{b:02x}").chars() {
                                out.push(c as u32);
                            }
                        }
                    }
                    _ => {
                        return Err(crate::error::lookup_error(format!(
                            "unknown error handler name '{errors}'"
                        )));
                    }
                }
                i = bytes.len();
                break;
            }
            match mb_to_wc(cp, MB_ERR_INVALID_CHARS, &bytes[i..i + insize]) {
                Ok(w) => {
                    out.extend(utf16_to_cps(&w));
                    i += insize;
                }
                Err(_) => {
                    // CPython reports a one-byte error range and resumes
                    // after the failing byte.
                    match errors {
                        "strict" => {
                            return Err(crate::error::unicode_decode_error(
                                &name,
                                bytes,
                                i,
                                i + 1,
                                DECODE_REASON,
                            ));
                        }
                        "ignore" => {}
                        "replace" => out.push(0xFFFD),
                        "surrogateescape" => out.push(0xDC00 + u32::from(bytes[i])),
                        "backslashreplace" => {
                            for c in format!("\\x{:02x}", bytes[i]).chars() {
                                out.push(c as u32);
                            }
                        }
                        _ => {
                            return Err(crate::error::lookup_error(format!(
                                "unknown error handler name '{errors}'"
                            )));
                        }
                    }
                    i += 1;
                }
            }
        }
        Ok((out, i))
    }

    pub(super) fn decode_impl(
        cp: u32,
        bytes: &[u8],
        errors: &str,
        final_: bool,
    ) -> Result<(Object, usize), RuntimeError> {
        if bytes.is_empty() {
            return Ok((Object::from_static(""), 0));
        }
        // Strict whole-buffer pass. `MB_ERR_INVALID_CHARS` is rejected by
        // CP_UTF7 (ERROR_INVALID_FLAGS) — CPython treats UTF-7 decoding
        // through its own codec, so a flagless retry is a fair fallback.
        let flags = if cp == CP_UTF7 {
            0
        } else {
            MB_ERR_INVALID_CHARS
        };
        // An incomplete trailing DBCS sequence makes the strict pass fail
        // with ERROR_NO_UNICODE_TRANSLATION, so the per-sequence path below
        // handles both genuine mojibake and `final=False` truncation.
        match mb_to_wc(cp, flags, bytes) {
            Ok(w) => {
                // The strict pass decoded everything — but with
                // `final=False` a trailing lead byte must stay buffered even
                // if the code page happens to also map it standalone.
                if !final_ && unsafe { IsDBCSLeadByteEx(cp, bytes[bytes.len() - 1]) } != 0 {
                    let (cps, consumed) = decode_errors(cp, bytes, errors, final_)?;
                    return Ok((Object::str_from_codepoints(cps), consumed));
                }
                Ok((Object::str_from_codepoints(utf16_to_cps(&w)), bytes.len()))
            }
            Err(e) if e == ERROR_NO_UNICODE_TRANSLATION => {
                let (cps, consumed) = decode_errors(cp, bytes, errors, final_)?;
                Ok((Object::str_from_codepoints(cps), consumed))
            }
            Err(e) => Err(crate::stdlib::nt_support::win32_error_to_py(e as i32, None)),
        }
    }

    /// CPython `encode_code_page_errors`: encode character by character,
    /// running the error handler wherever the code page has no mapping.
    fn encode_errors(cp: u32, cps: &[u32], errors: &str) -> Result<Vec<u8>, RuntimeError> {
        let name = code_page_name(cp);
        let mut out: Vec<u8> = Vec::with_capacity(cps.len());
        for (pos, &cp_ch) in cps.iter().enumerate() {
            let wide = cps_to_utf16(&[cp_ch]);
            let is_surrogate = (0xD800..0xE000).contains(&cp_ch);
            let encoded = if is_surrogate {
                None
            } else {
                let mut used_default = false;
                match wc_to_mb(cp, &wide, &mut used_default) {
                    Ok(b) if !used_default => Some(b),
                    _ => None,
                }
            };
            match encoded {
                Some(b) => out.extend_from_slice(&b),
                None => match errors {
                    "strict" => {
                        return Err(crate::error::unicode_encode_error_obj(
                            &name,
                            Object::str_from_codepoints(cps.to_vec()),
                            pos,
                            pos + 1,
                            ENCODE_REASON,
                        ));
                    }
                    "ignore" => {}
                    "replace" => out.push(b'?'),
                    "backslashreplace" => {
                        let esc = if cp_ch <= 0xFF {
                            format!("\\x{cp_ch:02x}")
                        } else if cp_ch <= 0xFFFF {
                            format!("\\u{cp_ch:04x}")
                        } else {
                            format!("\\U{cp_ch:08x}")
                        };
                        out.extend_from_slice(esc.as_bytes());
                    }
                    "xmlcharrefreplace" => {
                        out.extend_from_slice(format!("&#{cp_ch};").as_bytes());
                    }
                    "surrogateescape" if (0xDC80..=0xDCFF).contains(&cp_ch) => {
                        out.push((cp_ch - 0xDC00) as u8);
                    }
                    "surrogateescape" => {
                        return Err(crate::error::unicode_encode_error_obj(
                            &name,
                            Object::str_from_codepoints(cps.to_vec()),
                            pos,
                            pos + 1,
                            ENCODE_REASON,
                        ));
                    }
                    _ => {
                        return Err(crate::error::lookup_error(format!(
                            "unknown error handler name '{errors}'"
                        )));
                    }
                },
            }
        }
        Ok(out)
    }

    pub(super) fn encode_impl(cp: u32, cps: &[u32], errors: &str) -> Result<Vec<u8>, RuntimeError> {
        if cps.is_empty() {
            return Ok(Vec::new());
        }
        // Lone surrogates can't survive the Win32 wide round trip; they must
        // take the per-character path (which raises or escapes them).
        let has_lone_surrogate = cps.iter().any(|&c| (0xD800..0xE000).contains(&c));
        if !has_lone_surrogate {
            let wide = cps_to_utf16(cps);
            let mut used_default = false;
            if let Ok(b) = wc_to_mb(cp, &wide, &mut used_default) {
                if !used_default {
                    return Ok(b);
                }
            }
        }
        encode_errors(cp, cps, errors)
    }

    /// The `code_page` int argument of `code_page_encode`/`code_page_decode`.
    fn arg_code_page(args: &[Object], idx: usize, name: &str) -> Result<u32, RuntimeError> {
        match args.get(idx) {
            Some(Object::Int(i)) => {
                u32::try_from(*i).map_err(|_| value_error(format!("invalid code page number {i}")))
            }
            Some(o) => Err(type_error(format!(
                "{name}() argument 'code_page' must be int, not {}",
                o.type_name()
            ))),
            None => Err(type_error(format!("{name}() missing required argument"))),
        }
    }

    pub(super) fn b_code_page_encode(args: &[Object]) -> Result<Object, RuntimeError> {
        let cp = arg_code_page(args, 0, "code_page_encode")?;
        let cps = arg_text_codepoints(args, 1, "code_page_encode")?;
        let errors = arg_errors(args, 2);
        Ok(enc_tuple(encode_impl(cp, &cps, &errors)?, cps.len()))
    }

    pub(super) fn b_code_page_decode(args: &[Object]) -> Result<Object, RuntimeError> {
        let cp = arg_code_page(args, 0, "code_page_decode")?;
        let bytes = arg_bytes(args, 1, "code_page_decode")?;
        let errors = arg_errors(args, 2);
        let final_ = arg_final(args, 3);
        let (text, consumed) = decode_impl(cp, &bytes, &errors, final_)?;
        Ok(dec_tuple(text, consumed))
    }

    pub(super) fn b_mbcs_encode(args: &[Object]) -> Result<Object, RuntimeError> {
        let cps = arg_text_codepoints(args, 0, "mbcs_encode")?;
        let errors = arg_errors(args, 1);
        Ok(enc_tuple(encode_impl(CP_ACP, &cps, &errors)?, cps.len()))
    }

    pub(super) fn b_mbcs_decode(args: &[Object]) -> Result<Object, RuntimeError> {
        let bytes = arg_bytes(args, 0, "mbcs_decode")?;
        let errors = arg_errors(args, 1);
        let final_ = arg_final(args, 2);
        let (text, consumed) = decode_impl(CP_ACP, &bytes, &errors, final_)?;
        Ok(dec_tuple(text, consumed))
    }

    pub(super) fn b_oem_encode(args: &[Object]) -> Result<Object, RuntimeError> {
        let cps = arg_text_codepoints(args, 0, "oem_encode")?;
        let errors = arg_errors(args, 1);
        Ok(enc_tuple(encode_impl(CP_OEMCP, &cps, &errors)?, cps.len()))
    }

    pub(super) fn b_oem_decode(args: &[Object]) -> Result<Object, RuntimeError> {
        let bytes = arg_bytes(args, 0, "oem_decode")?;
        let errors = arg_errors(args, 1);
        let final_ = arg_final(args, 2);
        let (text, consumed) = decode_impl(CP_OEMCP, &bytes, &errors, final_)?;
        Ok(dec_tuple(text, consumed))
    }
}
