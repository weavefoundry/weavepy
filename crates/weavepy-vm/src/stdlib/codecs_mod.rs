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
        // KOI8 Cyrillic — `encoding_rs` knows these, but only under the
        // hyphenated WHATWG labels our normaliser strips.
        "koi8r" | "cskoi8r" => Encoding::for_label(b"koi8-r"),
        "koi8u" => Encoding::for_label(b"koi8-u"),
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
            return Err(value_error(format!("'{encoding}' codec can't encode input")));
        }
        return Ok(bytes.into_owned());
    }
    // Native fast path doesn't know this encoding — consult the Python codec
    // registry (custom `codecs.register` codecs and the `encodings/*.py`
    // modules), mirroring CPython's C-fast-path/Python-registry split.
    if let Some(out) = encode_via_registry(s, encoding, errors)? {
        return Ok(out);
    }
    Err(crate::error::lookup_error(format!("unknown encoding: {encoding}")))
}

pub fn decode_bytes(bytes: &[u8], encoding: &str, errors: &str) -> Result<String, RuntimeError> {
    check_error_handler(errors)?;
    if let Some(out) = decode_special(bytes, encoding, errors)? {
        return Ok(out);
    }
    if let Some(enc) = lookup_encoding(encoding) {
        let (text, _, had_errors) = enc.decode(bytes);
        if had_errors && errors == "strict" {
            return Err(value_error(format!("'{encoding}' codec can't decode input")));
        }
        return Ok(text.into_owned());
    }
    if let Some(out) = decode_via_registry(bytes, encoding, errors)? {
        return Ok(out);
    }
    Err(crate::error::lookup_error(format!("unknown encoding: {encoding}")))
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
    let Some(codec) = registry_codec_attr(encoding, "decode")? else {
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
    let out = res?;
    let first = match &out {
        Object::Tuple(t) if !t.is_empty() => t[0].clone(),
        other => other.clone(),
    };
    match first {
        Object::Str(s) => Ok(Some(s.to_string())),
        // A codec was found and run, but returned a non-`str` result. This is
        // the `io.TextIOWrapper` read path consuming a binary-transform codec
        // (`quopri`/`hex`) whose `_is_text_encoding` guard was bypassed: CPython
        // raises `TypeError` from `textio.c`, not a `LookupError`
        // (`test_io.test_illegal_decoder`).
        other => Err(type_error(format!(
            "decoder should return a string result, not '{}'",
            other.type_name()
        ))),
    }
}

/// `encode` counterpart to [`decode_via_registry`].
fn encode_via_registry(
    s: &str,
    encoding: &str,
    errors: &str,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    let Some(codec) = registry_codec_attr(encoding, "encode")? else {
        return Ok(None);
    };
    let key = encoding.to_owned();
    REGISTRY_INFLIGHT.with(|st| st.borrow_mut().push(key.clone()));
    let res = with_interp(|interp| {
        interp.call_object(
            codec,
            &[Object::from_str(s), Object::from_str(errors)],
            &[],
        )
    });
    REGISTRY_INFLIGHT.with(|st| st.borrow_mut().retain(|e| e != &key));
    let out = res?;
    let first = match &out {
        Object::Tuple(t) if !t.is_empty() => t[0].clone(),
        other => other.clone(),
    };
    match first.as_bytes_view() {
        Some(b) => Ok(Some(b)),
        // Codec found and run, but returned a non-bytes result — the
        // `io.TextIOWrapper` write path over a binary-transform codec
        // (`rot13`) whose `_is_text_encoding` guard was bypassed. CPython's
        // `textio.c` raises `TypeError` (`test_io.test_illegal_encoder`).
        None => Err(type_error(format!(
            "encoder should return a bytes object, not '{}'",
            first.type_name()
        ))),
    }
}

/// Shared front half of the registry fallbacks: bail out (→ `Ok(None)`) when
/// there is no interpreter or the encoding is already being resolved, then
/// `codecs.lookup(encoding)` and return its `attr` (`"encode"`/`"decode"`)
/// callable. A `LookupError` from `lookup` is swallowed (→ `Ok(None)`).
fn registry_codec_attr(encoding: &str, attr: &str) -> Result<Option<Object>, RuntimeError> {
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
        match interp.load_attr_public(&info, attr) {
            Ok(c) => Ok(Some(c)),
            Err(_) => Ok(None),
        }
    })
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

/// Handle special-case encodings whose semantics don't quite match
/// `encoding_rs`'s default behaviour (utf-8 with `surrogateescape`,
/// latin-1, raw_unicode_escape, etc.).
fn encode_special(s: &str, encoding: &str, errors: &str) -> Result<Option<Vec<u8>>, RuntimeError> {
    let key = encoding_key(encoding);
    Ok(match key.as_str() {
        "utf8" => Some(encode_utf8(s, errors)?),
        "utf8sig" => {
            // UTF-8 with a leading BOM (CPython `utf_8_sig`). The stateless
            // codec always prepends the BOM; the BOM-once-per-stream nuance
            // lives in the incremental encoder (frozen `codecs.py`).
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend(encode_utf8(s, errors)?);
            Some(out)
        }
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
        "utf7" => Some(encode_utf7(s)),
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
        "rawunicodeescape" => Some(decode_raw_unicode_escape(bytes)?),
        "unicodeescape" => Some(decode_unicode_escape(bytes)?),
        "cp437" | "437" | "ibm437" => Some(decode_cp437(bytes)),
        "utf7" => Some(decode_utf7(bytes, errors)?),
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

// ---------- UTF-7 (RFC 2152) ----------
//
// `encoding_rs` has no UTF-7, but real code drives it (e.g. `tarfile` opened
// with `encoding='utf7'`). Ported faithfully from CPython 3.13's
// `_PyUnicode_EncodeUTF7` / `PyUnicode_DecodeUTF7Stateful`
// (Objects/unicodeobject.c). The stateless codec encodes the modified-Base64
// shifted sequences over UTF-16 code units. Because WeavePy's `str` is strict
// UTF-8, lone surrogates produced by malformed input become U+FFFD (the same
// concession the UTF-8 surrogateescape path makes).

const UTF7_BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// CPython `utf7_category`: 0=Set D, 1=Set O, 2=whitespace, 3=must-base64.
#[rustfmt::skip]
const UTF7_CATEGORY: [u8; 128] = [
    3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 3, 3, 2, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    2, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 3, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0,
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 3, 1, 1, 1,
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 3, 3,
];

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

#[inline]
fn utf7_to_base64(n: u64) -> u8 {
    UTF7_BASE64[(n & 0x3f) as usize]
}

/// `DECODE_DIRECT`: an ASCII byte (other than `+`) that decodes as itself.
#[inline]
fn utf7_decode_direct(c: u32) -> bool {
    c <= 127 && c != u32::from(b'+')
}

/// `ENCODE_DIRECT(c, directO=1, directWS=1)` — the Python codec passes
/// `base64SetO=0`/`base64WhiteSpace=0`, so every ASCII char outside category 3
/// is emitted literally.
#[inline]
fn utf7_encode_direct(c: u32) -> bool {
    c > 0 && c < 128 && UTF7_CATEGORY[c as usize] != 3
}

/// Push a (possibly surrogate) code point; WeavePy `str` can't hold lone
/// surrogates, so they degrade to U+FFFD (see module note).
#[inline]
fn utf7_push(out: &mut String, cp: u32) {
    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
}

fn encode_utf7(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut in_shift = false;
    let mut base64bits: u32 = 0;
    let mut base64buffer: u64 = 0;
    for c in s.chars() {
        let mut ch = c as u32;
        if in_shift {
            if utf7_encode_direct(ch) {
                if base64bits > 0 {
                    out.push(utf7_to_base64(base64buffer << (6 - base64bits)));
                    base64buffer = 0;
                    base64bits = 0;
                }
                in_shift = false;
                if utf7_is_base64(ch) || ch == u32::from(b'-') {
                    out.push(b'-');
                }
                out.push(ch as u8);
                continue;
            }
            // else: fall through to encode_char.
        } else if ch == u32::from(b'+') {
            out.push(b'+');
            out.push(b'-');
            continue;
        } else if utf7_encode_direct(ch) {
            out.push(ch as u8);
            continue;
        } else {
            out.push(b'+');
            in_shift = true;
            // fall through to encode_char.
        }
        // encode_char: accumulate UTF-16 code unit(s) into the base64 buffer.
        if ch >= 0x10000 {
            let v = ch - 0x10000;
            let hi = 0xD800 + (v >> 10);
            base64bits += 16;
            base64buffer = (base64buffer << 16) | u64::from(hi);
            while base64bits >= 6 {
                out.push(utf7_to_base64(base64buffer >> (base64bits - 6)));
                base64bits -= 6;
            }
            ch = 0xDC00 + (v & 0x3FF);
        }
        base64bits += 16;
        base64buffer = (base64buffer << 16) | u64::from(ch);
        while base64bits >= 6 {
            out.push(utf7_to_base64(base64buffer >> (base64bits - 6)));
            base64bits -= 6;
        }
    }
    if base64bits > 0 {
        out.push(utf7_to_base64(base64buffer << (6 - base64bits)));
    }
    if in_shift {
        out.push(b'-');
    }
    out
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
