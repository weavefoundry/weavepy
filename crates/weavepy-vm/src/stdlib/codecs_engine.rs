//! Unified codec engine (RFC 0050 WS2).
//!
//! Faithful ports of CPython 3.13's UTF-8/16/32/7, ASCII and Latin-1
//! coders (`Objects/unicodeobject.c` + `Objects/stringlib/codecs.h`)
//! with the *unified error-handler protocol*: the fast path scans until
//! an error, then the handler is resolved **by name** — built-ins run
//! natively, anything else calls the callable registered through
//! `codecs.register_error` with a real `UnicodeDecodeError`/
//! `UnicodeEncodeError`, applies the `(replacement, new_position)`
//! result (including bytes replacements on encode and backward
//! positions), and the scan continues. Decoders speak the *stateful*
//! protocol: `final=false` leaves a trailing incomplete sequence
//! unconsumed and reports how many bytes were consumed.
//!
//! Text is processed as raw code points (`u32`, lone surrogates
//! allowed) so PEP 383 `surrogateescape` and `surrogatepass` round-trip
//! through [`Object::str_from_codepoints`]/[`Object::WStr`].

use crate::error::{type_error, RuntimeError};
use crate::object::Object;

// ---------------------------------------------------------------------------
// Handler resolution
// ---------------------------------------------------------------------------

/// A resolved error handler: a built-in dispatched natively, or a custom
/// callable registered via `codecs.register_error`.
#[derive(Clone)]
enum Handler {
    Strict,
    Ignore,
    Replace,
    BackslashReplace,
    XmlCharRefReplace,
    NameReplace,
    SurrogateEscape,
    SurrogatePass,
    Custom(Object),
}

fn resolve_handler(name: &str) -> Result<Handler, RuntimeError> {
    Ok(match name {
        "strict" => Handler::Strict,
        "ignore" => Handler::Ignore,
        "replace" => Handler::Replace,
        "backslashreplace" => Handler::BackslashReplace,
        "xmlcharrefreplace" => Handler::XmlCharRefReplace,
        "namereplace" => Handler::NameReplace,
        "surrogateescape" => Handler::SurrogateEscape,
        "surrogatepass" => Handler::SurrogatePass,
        _ => Handler::Custom(lookup_error_callable(name)?),
    })
}

/// `codecs.lookup_error(name)` through the live interpreter (custom
/// handlers live in the frozen `codecs.py` registry).
fn lookup_error_callable(name: &str) -> Result<Object, RuntimeError> {
    let unknown = || crate::error::lookup_error(format!("unknown error handler name '{name}'"));
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(unknown)?;
    // SAFETY: pointer published by an enclosing VM frame on this thread;
    // the GIL keeps the reentrant access exclusive (same contract as
    // `codecs_mod::with_interp`).
    let interp = unsafe { &mut *ptr };
    let codecs = interp.import_path("codecs").map_err(|_| unknown())?;
    let lookup = interp
        .load_attr_public(&codecs, "lookup_error")
        .map_err(|_| unknown())?;
    interp.call_object(lookup, &[Object::from_str(name)], &[])
}

/// The byte-order-resolved "standard" encoding name used in error payloads
/// (`utf-16-le`, not `utf-16`) — CPython names the concrete codec.
fn call_custom(handler: &Object, exc: &Object) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: see `lookup_error_callable`.
    let interp = unsafe { &mut *ptr };
    interp.call_object(handler.clone(), &[exc.clone()], &[])
}

fn get_attr(obj: &Object, name: &str) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: see `lookup_error_callable`.
    let interp = unsafe { &mut *ptr };
    interp.load_attr_public(obj, name)
}

/// Code points of any string object (`Str` fast path, `WStr` raw points).
pub fn str_codepoints(obj: &Object) -> Option<Vec<u32>> {
    match obj {
        Object::Str(s) => Some(s.chars().map(|c| c as u32).collect()),
        Object::WStr(cps) => Some(cps.to_vec()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Decode driver
// ---------------------------------------------------------------------------

/// Mutable decode context threaded through a decoder loop. The handler may
/// *replace the input* (by assigning `exc.object`), so `input` is owned.
struct DecCtx {
    encoding: String,
    errors: String,
    handler: Option<Handler>,
    input: Vec<u8>,
}

impl DecCtx {
    fn new(encoding: &str, errors: &str, input: &[u8]) -> Self {
        DecCtx {
            encoding: encoding.to_owned(),
            errors: errors.to_owned(),
            handler: None,
            input: input.to_vec(),
        }
    }

    fn handler(&mut self) -> Result<Handler, RuntimeError> {
        if self.handler.is_none() {
            self.handler = Some(resolve_handler(&self.errors)?);
        }
        Ok(self.handler.clone().unwrap())
    }

    fn error_object(&self, start: usize, end: usize, reason: &str) -> Object {
        crate::builtin_types::make_unicode_decode_error(
            &self.encoding,
            &self.input,
            start,
            end,
            reason,
        )
    }

    /// Resolve a decode error over `input[start..end]`; extends `out` with
    /// the replacement and returns the position to continue scanning from.
    fn on_error(
        &mut self,
        out: &mut Vec<u32>,
        start: usize,
        end: usize,
        reason: &str,
    ) -> Result<usize, RuntimeError> {
        match self.handler()? {
            Handler::Strict => Err(RuntimeError::PyException(crate::error::PyException::new(
                self.error_object(start, end, reason),
            ))),
            Handler::Ignore => Ok(end),
            Handler::Replace => {
                out.push(0xFFFD);
                Ok(end)
            }
            Handler::BackslashReplace => {
                for &b in &self.input[start..end] {
                    out.extend(format!("\\x{b:02x}").chars().map(|c| c as u32));
                }
                Ok(end)
            }
            Handler::SurrogateEscape => {
                for &b in &self.input[start..end] {
                    if b < 128 {
                        // PEP 383 only smuggles bytes >= 0x80; an ASCII byte
                        // in an illegal sequence is a hard error.
                        return Err(RuntimeError::PyException(crate::error::PyException::new(
                            self.error_object(start, end, reason),
                        )));
                    }
                    out.push(0xDC00 + u32::from(b));
                }
                Ok(end)
            }
            Handler::SurrogatePass => {
                // Accept exactly the CESU-8 / UTF-16 / UTF-32 encoded lone
                // surrogate at `start`; anything else is the original error.
                if let Some((cp, len)) =
                    decode_encoded_surrogate(&self.encoding, &self.input[start..])
                {
                    out.push(cp);
                    Ok(start + len)
                } else {
                    Err(RuntimeError::PyException(crate::error::PyException::new(
                        self.error_object(start, end, reason),
                    )))
                }
            }
            Handler::XmlCharRefReplace | Handler::NameReplace => Err(type_error(
                "don't know how to handle UnicodeDecodeError in error callback",
            )),
            Handler::Custom(h) => {
                let exc = self.error_object(start, end, reason);
                let res = call_custom(&h, &exc)?;
                let items: Vec<Object> = match &res {
                    Object::Tuple(t) if t.len() == 2 => t.to_vec(),
                    _ => {
                        return Err(type_error(
                            "decoding error handler must return (str, int) tuple",
                        ))
                    }
                };
                let rep = str_codepoints(&items[0]).ok_or_else(|| {
                    type_error("decoding error handler must return (str, int) tuple")
                })?;
                let mut newpos = match &items[1] {
                    Object::Int(i) => *i,
                    _ => {
                        return Err(type_error(
                            "decoding error handler must return (str, int) tuple",
                        ))
                    }
                };
                // The handler may have replaced `exc.object`; re-fetch it
                // (CPython raises TypeError when it's no longer bytes —
                // ignoring the mutation would loop forever).
                let o = get_attr(&exc, "object")?;
                self.input = o
                    .as_bytes_view()
                    .ok_or_else(|| type_error("object attribute must be bytes"))?;
                let insize = self.input.len() as i64;
                if newpos < 0 {
                    newpos += insize;
                }
                if newpos < 0 || newpos > insize {
                    return Err(crate::error::index_error(format!(
                        "position {newpos} from error handler out of bounds"
                    )));
                }
                out.extend(rep);
                Ok(newpos as usize)
            }
        }
    }
}

/// Decode the lone-surrogate byte sequence at the front of `rest` for the
/// `surrogatepass` handler. Returns `(code point, bytes consumed)`.
fn decode_encoded_surrogate(encoding: &str, rest: &[u8]) -> Option<(u32, usize)> {
    let key = standard_key(encoding);
    match key {
        "utf8" => {
            if rest.len() >= 3
                && rest[0] == 0xED
                && (0xA0..=0xBF).contains(&rest[1])
                && (0x80..=0xBF).contains(&rest[2])
            {
                let cp = ((u32::from(rest[0]) & 0x0F) << 12)
                    | ((u32::from(rest[1]) & 0x3F) << 6)
                    | (u32::from(rest[2]) & 0x3F);
                Some((cp, 3))
            } else {
                None
            }
        }
        "utf16le" | "utf16be" => {
            if rest.len() >= 2 {
                let u = if key == "utf16le" {
                    u16::from_le_bytes([rest[0], rest[1]])
                } else {
                    u16::from_be_bytes([rest[0], rest[1]])
                };
                if (0xD800..=0xDFFF).contains(&u) {
                    return Some((u32::from(u), 2));
                }
            }
            None
        }
        "utf32le" | "utf32be" => {
            if rest.len() >= 4 {
                let v = if key == "utf32le" {
                    u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]])
                } else {
                    u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]])
                };
                if (0xD800..=0xDFFF).contains(&v) {
                    return Some((v, 4));
                }
            }
            None
        }
        _ => None,
    }
}

/// Normalise an error-payload encoding name to a family key for
/// `surrogatepass` (mirrors CPython's `get_standard_encoding`).
fn standard_key(encoding: &str) -> &'static str {
    let norm: String = encoding
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match norm.as_str() {
        "utf8" | "utf8sig" | "cp65001" => "utf8",
        "utf16le" => "utf16le",
        "utf16be" => "utf16be",
        // Byte-order-less names resolve to native (little-endian) order.
        "utf16" | "u16" => "utf16le",
        "utf32le" => "utf32le",
        "utf32be" => "utf32be",
        "utf32" | "u32" => "utf32le",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// unicode-escape / raw-unicode-escape decoders
// ---------------------------------------------------------------------------

/// Faithful port of CPython's `_PyUnicode_DecodeUnicodeEscapeInternal2`.
/// Returns `(text, consumed, first_invalid_escape_warning)` — the warning
/// message (e.g. ``invalid escape sequence '\z'``) that the caller emits as
/// a `DeprecationWarning`, or `None`.
pub fn unicode_escape_decode(
    data: &[u8],
    errors: &str,
    final_: bool,
) -> Result<(Object, usize, Option<String>), RuntimeError> {
    let mut ctx = DecCtx::new("unicodeescape", errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    let mut warn: Option<String> = None;
    let mut consumed = data.len();
    let hexval = |c: u8| -> Option<u32> {
        match c {
            b'0'..=b'9' => Some(u32::from(c - b'0')),
            b'a'..=b'f' => Some(u32::from(c - b'a') + 10),
            b'A'..=b'F' => Some(u32::from(c - b'A') + 10),
            _ => None,
        }
    };
    let mut i = 0usize;
    'outer: while i < ctx.input.len() {
        let c = ctx.input[i];
        i += 1;
        if c != b'\\' {
            out.push(u32::from(c));
            continue;
        }
        let start = i - 1;
        if i >= ctx.input.len() {
            if !final_ {
                consumed = start;
                break;
            }
            i = ctx.on_error(&mut out, start, i, "\\ at end of string")?;
            continue;
        }
        let c = ctx.input[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(u32::from(b'\\')),
            b'\'' => out.push(u32::from(b'\'')),
            b'"' => out.push(u32::from(b'"')),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b't' => out.push(u32::from(b'\t')),
            b'n' => out.push(u32::from(b'\n')),
            b'r' => out.push(u32::from(b'\r')),
            b'v' => out.push(0x0B),
            b'a' => out.push(0x07),
            b'0'..=b'7' => {
                let mut ch = u32::from(c - b'0');
                if i < ctx.input.len() && (b'0'..=b'7').contains(&ctx.input[i]) {
                    ch = (ch << 3) + u32::from(ctx.input[i] - b'0');
                    i += 1;
                    if i < ctx.input.len() && (b'0'..=b'7').contains(&ctx.input[i]) {
                        ch = (ch << 3) + u32::from(ctx.input[i] - b'0');
                        i += 1;
                    }
                }
                if ch > 0o377 && warn.is_none() {
                    warn = Some(format!("invalid octal escape sequence '\\{ch:o}'"));
                }
                out.push(ch);
            }
            b'x' | b'u' | b'U' => {
                let (count, message) = match c {
                    b'x' => (2usize, "truncated \\xXX escape"),
                    b'u' => (4, "truncated \\uXXXX escape"),
                    _ => (8, "truncated \\UXXXXXXXX escape"),
                };
                let mut ch: u32 = 0;
                let mut k = 0usize;
                loop {
                    if k == count {
                        break;
                    }
                    if i >= ctx.input.len() {
                        if !final_ {
                            consumed = start;
                            break 'outer;
                        }
                        i = ctx.on_error(&mut out, start, i, message)?;
                        continue 'outer;
                    }
                    match hexval(ctx.input[i]) {
                        Some(v) => ch = (ch << 4) + v,
                        None => {
                            i = ctx.on_error(&mut out, start, i, message)?;
                            continue 'outer;
                        }
                    }
                    i += 1;
                    k += 1;
                }
                if ch > 0x0010_FFFF {
                    i = ctx.on_error(&mut out, start, i, "illegal Unicode character")?;
                    continue;
                }
                out.push(ch);
            }
            b'N' => {
                let message = "malformed \\N character escape";
                if i >= ctx.input.len() {
                    if !final_ {
                        consumed = start;
                        break;
                    }
                    i = ctx.on_error(&mut out, start, i, message)?;
                    continue;
                }
                if ctx.input[i] == b'{' {
                    let name_start = i + 1;
                    let mut j = name_start;
                    while j < ctx.input.len() && ctx.input[j] != b'}' {
                        j += 1;
                    }
                    if j >= ctx.input.len() {
                        if !final_ {
                            consumed = start;
                            break;
                        }
                        i = ctx.on_error(&mut out, start, j, message)?;
                        continue;
                    }
                    if j > name_start {
                        let name = String::from_utf8_lossy(&ctx.input[name_start..j]).into_owned();
                        i = j + 1;
                        if let Some(ch) = crate::stdlib::unicodedata_mod::name_to_char(&name) {
                            out.push(ch as u32);
                            continue;
                        }
                        i = ctx.on_error(&mut out, start, i, "unknown Unicode character name")?;
                        continue;
                    }
                    i = j;
                }
                i = ctx.on_error(&mut out, start, i, message)?;
            }
            _ => {
                if warn.is_none() {
                    warn = Some(format!("invalid escape sequence '\\{}'", c as char));
                }
                out.push(u32::from(b'\\'));
                out.push(u32::from(c));
            }
        }
    }
    Ok((Object::str_from_codepoints(out), consumed, warn))
}

/// Faithful port of CPython's `_PyUnicode_DecodeRawUnicodeEscapeStateful`.
pub fn raw_unicode_escape_decode(
    data: &[u8],
    errors: &str,
    final_: bool,
) -> Result<(Object, usize), RuntimeError> {
    let mut ctx = DecCtx::new("rawunicodeescape", errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    let mut consumed = data.len();
    let hexval = |c: u8| -> Option<u32> {
        match c {
            b'0'..=b'9' => Some(u32::from(c - b'0')),
            b'a'..=b'f' => Some(u32::from(c - b'a') + 10),
            b'A'..=b'F' => Some(u32::from(c - b'A') + 10),
            _ => None,
        }
    };
    let mut i = 0usize;
    'outer: while i < ctx.input.len() {
        let c = ctx.input[i];
        i += 1;
        // A non-backslash — or a trailing backslash in final mode — is a
        // Unicode ordinal (latin-1 identity).
        if c != b'\\' || (i >= ctx.input.len() && final_) {
            out.push(u32::from(c));
            continue;
        }
        let start = i - 1;
        if i >= ctx.input.len() {
            // !final: leave the trailing backslash unconsumed.
            consumed = start;
            break;
        }
        let c = ctx.input[i];
        i += 1;
        let (count, message) = match c {
            b'u' => (4usize, "truncated \\uXXXX escape"),
            b'U' => (8, "truncated \\UXXXXXXXX escape"),
            _ => {
                out.push(u32::from(b'\\'));
                out.push(u32::from(c));
                continue;
            }
        };
        let mut ch: u32 = 0;
        let mut k = 0usize;
        loop {
            if k == count {
                break;
            }
            if i >= ctx.input.len() {
                if !final_ {
                    consumed = start;
                    break 'outer;
                }
                i = ctx.on_error(&mut out, start, i, message)?;
                continue 'outer;
            }
            match hexval(ctx.input[i]) {
                Some(v) => ch = (ch << 4) + v,
                None => {
                    i = ctx.on_error(&mut out, start, i, message)?;
                    continue 'outer;
                }
            }
            i += 1;
            k += 1;
        }
        if ch > 0x0010_FFFF {
            i = ctx.on_error(&mut out, start, i, "\\Uxxxxxxxx out of range")?;
            continue;
        }
        out.push(ch);
    }
    Ok((Object::str_from_codepoints(out), consumed))
}

// ---------------------------------------------------------------------------
// Charmap (single-byte) codecs
// ---------------------------------------------------------------------------

/// Sentinel for an unmapped position in a 256-entry charmap decode table
/// (CPython's `Lib/encodings/*.py` tables use `'\ufffe'` the same way).
pub const CHARMAP_UNDEFINED: u32 = 0xFFFE;

/// Charmap decode: `table[b]` is the code point for byte `b`,
/// [`CHARMAP_UNDEFINED`] marks an unmapped position. Runs the full error
/// handler protocol (codec name `'charmap'`, like CPython's
/// `charmap_decode`).
pub fn charmap_decode_table(
    data: &[u8],
    errors: &str,
    table: &[u32; 256],
) -> Result<(Object, usize), RuntimeError> {
    let mut ctx = DecCtx::new("charmap", errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    let mut i = 0usize;
    while i < ctx.input.len() {
        let cp = table[ctx.input[i] as usize];
        if cp == CHARMAP_UNDEFINED {
            i = ctx.on_error(&mut out, i, i + 1, "character maps to <undefined>")?;
        } else {
            out.push(cp);
            i += 1;
        }
    }
    let consumed = ctx.input.len();
    Ok((Object::str_from_codepoints(out), consumed))
}

/// Charmap encode via the reverse of a 256-entry decode table. Runs the
/// full error handler protocol; handler text replacements are re-encoded
/// through the same charmap (CPython `charmap_encode` semantics).
pub fn charmap_encode_table(
    cps: &[u32],
    errors: &str,
    table: &[u32; 256],
) -> Result<Vec<u8>, RuntimeError> {
    let mut rev = std::collections::HashMap::with_capacity(256);
    for (b, &cp) in table.iter().enumerate() {
        if cp != CHARMAP_UNDEFINED {
            rev.entry(cp).or_insert(b as u8);
        }
    }
    let mut ctx = EncCtx::new("charmap", errors, cps);
    encode_loop(
        &mut ctx,
        "character maps to <undefined>",
        |cp, out| match rev.get(&cp) {
            Some(&b) => {
                out.push(b);
                true
            }
            None => false,
        },
    )
}

// ---------------------------------------------------------------------------
// UTF-8 decoder
// ---------------------------------------------------------------------------

/// CPython's stateful UTF-8 decode: returns the decoded text and the number
/// of input bytes consumed (< len when `!final_` and the input ends in an
/// incomplete sequence).
pub fn utf8_decode(
    data: &[u8],
    errors: &str,
    final_: bool,
) -> Result<(Object, usize), RuntimeError> {
    utf8_decode_named(data, errors, final_, "utf-8")
}

/// [`utf8_decode`] with an explicit error-payload encoding name (the
/// `utf-8-sig` wrapper reports errors as `utf-8-sig`... CPython reports
/// plain `utf-8`, so this is used for both).
fn utf8_decode_named(
    data: &[u8],
    errors: &str,
    final_: bool,
    name: &str,
) -> Result<(Object, usize), RuntimeError> {
    let mut ctx = DecCtx::new(name, errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    let mut i = 0usize;
    loop {
        let s = &ctx.input;
        let n = s.len();
        if i >= n {
            break;
        }
        let b = s[i];
        if b < 0x80 {
            out.push(u32::from(b));
            i += 1;
            continue;
        }
        // Multibyte sequence — port of `stringlib/codecs.h utf8_decode`.
        enum Scan {
            Ok(u32, usize),
            Err(usize, &'static str), // span, reason
            Incomplete(usize),        // bytes available in the partial seq
        }
        let is_cont = |x: u8| (0x80..=0xBF).contains(&x);
        let scan = if b < 0xC2 {
            // \x80-\xBF stray continuation, \xC0-\xC1 overlong.
            Scan::Err(1, "invalid start byte")
        } else if b < 0xE0 {
            if i + 1 >= n {
                Scan::Incomplete(1)
            } else if !is_cont(s[i + 1]) {
                Scan::Err(1, "invalid continuation byte")
            } else {
                Scan::Ok(
                    ((u32::from(b) & 0x1F) << 6) | (u32::from(s[i + 1]) & 0x3F),
                    2,
                )
            }
        } else if b < 0xF0 {
            if i + 1 >= n {
                Scan::Incomplete(1)
            } else {
                let b2 = s[i + 1];
                if !is_cont(b2) || (if b2 < 0xA0 { b == 0xE0 } else { b == 0xED }) {
                    Scan::Err(1, "invalid continuation byte")
                } else if i + 2 >= n {
                    Scan::Incomplete(2)
                } else if !is_cont(s[i + 2]) {
                    Scan::Err(2, "invalid continuation byte")
                } else {
                    Scan::Ok(
                        ((u32::from(b) & 0x0F) << 12)
                            | ((u32::from(b2) & 0x3F) << 6)
                            | (u32::from(s[i + 2]) & 0x3F),
                        3,
                    )
                }
            }
        } else if b < 0xF5 {
            if i + 1 >= n {
                Scan::Incomplete(1)
            } else {
                let b2 = s[i + 1];
                if !is_cont(b2) || (if b2 < 0x90 { b == 0xF0 } else { b == 0xF4 }) {
                    Scan::Err(1, "invalid continuation byte")
                } else if i + 2 >= n {
                    Scan::Incomplete(2)
                } else if !is_cont(s[i + 2]) {
                    Scan::Err(2, "invalid continuation byte")
                } else if i + 3 >= n {
                    Scan::Incomplete(3)
                } else if !is_cont(s[i + 3]) {
                    Scan::Err(3, "invalid continuation byte")
                } else {
                    Scan::Ok(
                        ((u32::from(b) & 0x07) << 18)
                            | ((u32::from(b2) & 0x3F) << 12)
                            | ((u32::from(s[i + 2]) & 0x3F) << 6)
                            | (u32::from(s[i + 3]) & 0x3F),
                        4,
                    )
                }
            }
        } else {
            Scan::Err(1, "invalid start byte")
        };
        match scan {
            Scan::Ok(cp, len) => {
                out.push(cp);
                i += len;
            }
            Scan::Incomplete(avail) => {
                if !final_ {
                    // Leave the partial sequence for the next chunk.
                    return Ok((Object::str_from_codepoints(out), i));
                }
                let end = i + avail;
                i = ctx.on_error(
                    &mut out,
                    i,
                    end.min(ctx.input.len()),
                    "unexpected end of data",
                )?;
            }
            Scan::Err(span, reason) => {
                // CPython special case: a truncated surrogate `ED A0-BF` at
                // the very end of a non-final chunk is *incomplete*, not an
                // error (the next chunk may extend it... it can't become
                // valid, but `surrogatepass` needs the full 3 bytes).
                if !final_
                    && b == 0xED
                    && i + 2 == ctx.input.len()
                    && (0xA0..=0xBF).contains(&ctx.input[i + 1])
                {
                    return Ok((Object::str_from_codepoints(out), i));
                }
                i = ctx.on_error(&mut out, i, i + span, reason)?;
            }
        }
    }
    let consumed = ctx.input.len();
    Ok((Object::str_from_codepoints(out), consumed))
}

// ---------------------------------------------------------------------------
// UTF-16 / UTF-32 decoders
// ---------------------------------------------------------------------------

/// CPython's `PyUnicode_DecodeUTF16Stateful`: `byteorder` < 0 is LE, > 0 is
/// BE, 0 sniffs a BOM (defaulting to native/LE). Returns the decoded text,
/// bytes consumed, and the (possibly BOM-updated) byte order.
pub fn utf16_decode(
    data: &[u8],
    errors: &str,
    mut byteorder: i32,
    final_: bool,
) -> Result<(Object, usize, i32), RuntimeError> {
    let mut start = 0usize;
    if byteorder == 0 && data.len() >= 2 {
        let bom = (u32::from(data[1]) << 8) | u32::from(data[0]);
        if bom == 0xFEFF {
            start = 2;
            byteorder = -1;
        } else if bom == 0xFFFE {
            start = 2;
            byteorder = 1;
        }
    }
    let big = byteorder > 0;
    let encoding = if big { "utf-16-be" } else { "utf-16-le" };
    let mut ctx = DecCtx::new(encoding, errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len() / 2);
    let mut i = start;
    loop {
        let s = &ctx.input;
        let n = s.len();
        if i >= n {
            break;
        }
        if n - i < 2 {
            // Odd trailing byte.
            if !final_ {
                return Ok((Object::str_from_codepoints(out), i, byteorder));
            }
            i = ctx.on_error(&mut out, i, n, "truncated data")?;
            continue;
        }
        let rd = |s: &[u8], j: usize| -> u32 {
            if big {
                (u32::from(s[j]) << 8) | u32::from(s[j + 1])
            } else {
                (u32::from(s[j + 1]) << 8) | u32::from(s[j])
            }
        };
        let u = rd(s, i);
        if !(0xD800..=0xDFFF).contains(&u) {
            out.push(u);
            i += 2;
            continue;
        }
        if u >= 0xDC00 {
            // Lone low surrogate first: "illegal encoding", span 2.
            i = ctx.on_error(&mut out, i, i + 2, "illegal encoding")?;
            continue;
        }
        // High surrogate: need the pair.
        if n - i < 4 {
            if !final_ {
                return Ok((Object::str_from_codepoints(out), i, byteorder));
            }
            i = ctx.on_error(&mut out, i, n, "unexpected end of data")?;
            continue;
        }
        let u2 = rd(s, i + 2);
        if (0xDC00..=0xDFFF).contains(&u2) {
            out.push(0x1_0000 + ((u - 0xD800) << 10) + (u2 - 0xDC00));
            i += 4;
        } else {
            i = ctx.on_error(&mut out, i, i + 2, "illegal UTF-16 surrogate")?;
        }
    }
    let consumed = ctx.input.len();
    Ok((Object::str_from_codepoints(out), consumed, byteorder))
}

/// CPython's `PyUnicode_DecodeUTF32Stateful` (same byteorder protocol).
pub fn utf32_decode(
    data: &[u8],
    errors: &str,
    mut byteorder: i32,
    final_: bool,
) -> Result<(Object, usize, i32), RuntimeError> {
    let mut start = 0usize;
    if byteorder == 0 && data.len() >= 4 {
        if data[..4] == [0xFF, 0xFE, 0x00, 0x00] {
            start = 4;
            byteorder = -1;
        } else if data[..4] == [0x00, 0x00, 0xFE, 0xFF] {
            start = 4;
            byteorder = 1;
        }
    }
    let big = byteorder > 0;
    let encoding = if big { "utf-32-be" } else { "utf-32-le" };
    let mut ctx = DecCtx::new(encoding, errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len() / 4);
    let mut i = start;
    loop {
        let s = &ctx.input;
        let n = s.len();
        if i >= n {
            break;
        }
        if n - i < 4 {
            if !final_ {
                return Ok((Object::str_from_codepoints(out), i, byteorder));
            }
            i = ctx.on_error(&mut out, i, n, "truncated data")?;
            continue;
        }
        let v = if big {
            u32::from_be_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]])
        } else {
            u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]])
        };
        if (0xD800..=0xDFFF).contains(&v) {
            i = ctx.on_error(
                &mut out,
                i,
                i + 4,
                "code point in surrogate code point range(0xd800, 0xe000)",
            )?;
        } else if v < 0x11_0000 {
            out.push(v);
            i += 4;
        } else {
            i = ctx.on_error(&mut out, i, i + 4, "code point not in range(0x110000)")?;
        }
    }
    let consumed = ctx.input.len();
    Ok((Object::str_from_codepoints(out), consumed, byteorder))
}

// ---------------------------------------------------------------------------
// ASCII / Latin-1 decoders
// ---------------------------------------------------------------------------

pub fn ascii_decode(data: &[u8], errors: &str) -> Result<(Object, usize), RuntimeError> {
    let mut ctx = DecCtx::new("ascii", errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    let mut i = 0usize;
    while i < ctx.input.len() {
        let b = ctx.input[i];
        if b < 0x80 {
            out.push(u32::from(b));
            i += 1;
        } else {
            i = ctx.on_error(&mut out, i, i + 1, "ordinal not in range(128)")?;
        }
    }
    let consumed = ctx.input.len();
    Ok((Object::str_from_codepoints(out), consumed))
}

pub fn latin1_decode(data: &[u8], _errors: &str) -> Result<(Object, usize), RuntimeError> {
    // Latin-1 decode can never fail: bytes are ordinals.
    let s: String = data.iter().map(|&b| b as char).collect();
    Ok((Object::from_str(s), data.len()))
}

// ---------------------------------------------------------------------------
// UTF-7 decoder
// ---------------------------------------------------------------------------

const fn is_base64(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'+' || c == b'/'
}

fn from_base64(c: u8) -> u32 {
    match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' => 62,
        _ => 63,
    }
}

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Port of `PyUnicode_DecodeUTF7Stateful`.
pub fn utf7_decode(
    data: &[u8],
    errors: &str,
    final_: bool,
) -> Result<(Object, usize), RuntimeError> {
    let mut ctx = DecCtx::new("utf7", errors, data);
    let mut out: Vec<u32> = Vec::with_capacity(data.len());

    let mut in_shift = false;
    let mut shift_out_start = 0usize; // output length at shift start
    let mut start_in_pos = 0usize; // input index of the '+' opening the shift
    let mut base64bits: u32 = 0;
    let mut base64buffer: u64 = 0;
    let mut surrogate: u32 = 0;

    let mut i = 0usize;
    'outer: while i < ctx.input.len() {
        let ch = ctx.input[i];
        if in_shift {
            if is_base64(ch) {
                base64buffer = (base64buffer << 6) | u64::from(from_base64(ch));
                base64bits += 6;
                i += 1;
                if base64bits >= 16 {
                    let out_ch = ((base64buffer >> (base64bits - 16)) & 0xFFFF) as u32;
                    base64bits -= 16;
                    base64buffer &= (1u64 << base64bits) - 1;
                    if surrogate != 0 {
                        if (0xDC00..=0xDFFF).contains(&out_ch) {
                            out.push(0x1_0000 + ((surrogate - 0xD800) << 10) + (out_ch - 0xDC00));
                            surrogate = 0;
                            continue;
                        }
                        out.push(surrogate);
                        surrogate = 0;
                    }
                    if (0xD800..=0xDBFF).contains(&out_ch) {
                        surrogate = out_ch;
                    } else {
                        out.push(out_ch);
                    }
                }
            } else {
                // Leaving a base-64 section.
                in_shift = false;
                if base64bits > 0 {
                    if base64bits >= 6 {
                        i += 1;
                        base64bits = 0;
                        base64buffer = 0;
                        surrogate = 0;
                        i = ctx.on_error(
                            &mut out,
                            start_in_pos,
                            i,
                            "partial character in shift sequence",
                        )?;
                        continue;
                    } else if base64buffer != 0 {
                        i += 1;
                        base64bits = 0;
                        base64buffer = 0;
                        surrogate = 0;
                        i = ctx.on_error(
                            &mut out,
                            start_in_pos,
                            i,
                            "non-zero padding bits in shift sequence",
                        )?;
                        continue;
                    }
                }
                base64bits = 0;
                base64buffer = 0;
                if surrogate != 0 && ch <= 127 && ch != b'+' {
                    out.push(surrogate);
                }
                surrogate = 0;
                if ch == b'-' {
                    i += 1; // '-' is absorbed
                }
            }
        } else if ch == b'+' {
            start_in_pos = i;
            i += 1;
            if i < ctx.input.len() && ctx.input[i] == b'-' {
                i += 1;
                out.push(u32::from(b'+'));
            } else if i < ctx.input.len() && !is_base64(ctx.input[i]) {
                i += 1;
                i = ctx.on_error(&mut out, start_in_pos, i, "ill-formed sequence")?;
            } else {
                in_shift = true;
                surrogate = 0;
                shift_out_start = out.len();
                base64bits = 0;
                base64buffer = 0;
            }
        } else if ch <= 127 {
            out.push(u32::from(ch));
            i += 1;
        } else {
            start_in_pos = i;
            i += 1;
            i = ctx.on_error(&mut out, start_in_pos, i, "unexpected special character")?;
        }
        continue 'outer;
    }

    // End of input.
    if in_shift && final_ {
        in_shift = false;
        if surrogate != 0 || base64bits >= 6 || (base64bits > 0 && base64buffer != 0) {
            let end = ctx.input.len();
            i = ctx.on_error(&mut out, start_in_pos, end, "unterminated shift sequence")?;
            // A skipping handler may resume mid-input; keep it simple and
            // decode the remainder as direct characters.
            while i < ctx.input.len() {
                let ch = ctx.input[i];
                if ch <= 127 && ch != b'+' {
                    out.push(u32::from(ch));
                    i += 1;
                } else {
                    break;
                }
            }
        }
    }

    if in_shift {
        // Non-final chunk ends mid-shift: back off to the shift start.
        out.truncate(shift_out_start);
        return Ok((Object::str_from_codepoints(out), start_in_pos));
    }
    let consumed = ctx.input.len().max(i).min(ctx.input.len());
    Ok((Object::str_from_codepoints(out), consumed))
}

// ---------------------------------------------------------------------------
// Encode driver
// ---------------------------------------------------------------------------

/// Mutable encode context: the handler may replace `exc.object`.
struct EncCtx {
    encoding: String,
    errors: String,
    handler: Option<Handler>,
    input: Vec<u32>,
}

impl EncCtx {
    fn new(encoding: &str, errors: &str, input: &[u32]) -> Self {
        EncCtx {
            encoding: encoding.to_owned(),
            errors: errors.to_owned(),
            handler: None,
            input: input.to_vec(),
        }
    }

    fn handler(&mut self) -> Result<Handler, RuntimeError> {
        if self.handler.is_none() {
            self.handler = Some(resolve_handler(&self.errors)?);
        }
        Ok(self.handler.clone().unwrap())
    }

    fn error_object(&self, start: usize, end: usize, reason: &str) -> Object {
        crate::builtin_types::make_unicode_encode_error_obj(
            &self.encoding,
            Object::str_from_codepoints(self.input.clone()),
            start,
            end,
            reason,
        )
    }

    fn strict(&self, start: usize, end: usize, reason: &str) -> RuntimeError {
        RuntimeError::PyException(crate::error::PyException::new(
            self.error_object(start, end, reason),
        ))
    }

    /// Resolve an encode error over `input[start..end]`. Returns the
    /// replacement (text still to be encoded by the caller's codec, or raw
    /// bytes) and the position to continue from.
    fn on_error(
        &mut self,
        start: usize,
        end: usize,
        reason: &str,
    ) -> Result<(Replacement, usize), RuntimeError> {
        match self.handler()? {
            Handler::Strict => Err(self.strict(start, end, reason)),
            Handler::Ignore => Ok((Replacement::Bytes(Vec::new()), end)),
            // '?' per unencodable point, encoded through the codec (2 bytes
            // per '?' in UTF-16).
            Handler::Replace => Ok((Replacement::Ascii("?".repeat(end - start)), end)),
            Handler::BackslashReplace => {
                let mut s = String::new();
                for &cp in &self.input[start..end] {
                    if cp <= 0xFF {
                        s.push_str(&format!("\\x{cp:02x}"));
                    } else if cp <= 0xFFFF {
                        s.push_str(&format!("\\u{cp:04x}"));
                    } else {
                        s.push_str(&format!("\\U{cp:08x}"));
                    }
                }
                Ok((Replacement::Ascii(s), end))
            }
            Handler::XmlCharRefReplace => {
                let mut s = String::new();
                for &cp in &self.input[start..end] {
                    s.push_str(&format!("&#{cp};"));
                }
                Ok((Replacement::Ascii(s), end))
            }
            Handler::NameReplace => {
                let mut s = String::new();
                for &cp in &self.input[start..end] {
                    match char::from_u32(cp).and_then(crate::stdlib::unicodedata_mod::char_name) {
                        Some(name) => s.push_str(&format!("\\N{{{name}}}")),
                        None if cp <= 0xFF => s.push_str(&format!("\\x{cp:02x}")),
                        None if cp <= 0xFFFF => s.push_str(&format!("\\u{cp:04x}")),
                        None => s.push_str(&format!("\\U{cp:08x}")),
                    }
                }
                Ok((Replacement::Ascii(s), end))
            }
            Handler::SurrogateEscape => {
                let mut b = Vec::with_capacity(end - start);
                for (j, &cp) in self.input[start..end].iter().enumerate() {
                    if (0xDC80..=0xDCFF).contains(&cp) {
                        b.push((cp - 0xDC00) as u8);
                    } else {
                        // CPython smuggles the leading DC80-DCFF bytes inline
                        // and reports the error from the first non-smuggleable
                        // surrogate (`exc.start` past the handled prefix).
                        return Err(self.strict(start + j, end, reason));
                    }
                }
                Ok((Replacement::Bytes(b), end))
            }
            Handler::SurrogatePass => {
                let key = standard_key(&self.encoding);
                let mut b = Vec::new();
                for &cp in &self.input[start..end] {
                    if !(0xD800..=0xDFFF).contains(&cp) {
                        return Err(self.strict(start, end, reason));
                    }
                    match key {
                        "utf8" => {
                            b.push(0xE0 | (cp >> 12) as u8);
                            b.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                            b.push(0x80 | (cp & 0x3F) as u8);
                        }
                        "utf16le" => b.extend_from_slice(&(cp as u16).to_le_bytes()),
                        "utf16be" => b.extend_from_slice(&(cp as u16).to_be_bytes()),
                        "utf32le" => b.extend_from_slice(&cp.to_le_bytes()),
                        "utf32be" => b.extend_from_slice(&cp.to_be_bytes()),
                        _ => return Err(self.strict(start, end, reason)),
                    }
                }
                Ok((Replacement::Bytes(b), end))
            }
            Handler::Custom(h) => {
                let exc = self.error_object(start, end, reason);
                let res = call_custom(&h, &exc)?;
                let items: Vec<Object> = match &res {
                    Object::Tuple(t) if t.len() == 2 => t.to_vec(),
                    _ => {
                        return Err(type_error(
                            "encoding error handler must return (str/bytes, int) tuple",
                        ))
                    }
                };
                let rep = if let Some(cps) = str_codepoints(&items[0]) {
                    Replacement::Text(cps)
                } else if let Some(b) = items[0].as_bytes_view() {
                    Replacement::CustomBytes(b)
                } else {
                    return Err(type_error(
                        "encoding error handler must return (str/bytes, int) tuple",
                    ));
                };
                let mut newpos = match &items[1] {
                    Object::Int(i) => *i,
                    _ => {
                        return Err(type_error(
                            "encoding error handler must return (str/bytes, int) tuple",
                        ))
                    }
                };
                let o = get_attr(&exc, "object")?;
                self.input = str_codepoints(&o)
                    .ok_or_else(|| type_error("object attribute must be unicode"))?;
                let insize = self.input.len() as i64;
                if newpos < 0 {
                    newpos += insize;
                }
                if newpos < 0 || newpos > insize {
                    return Err(crate::error::index_error(format!(
                        "position {newpos} from error handler out of bounds"
                    )));
                }
                Ok((rep, newpos as usize))
            }
        }
    }
}

/// An encode error handler's replacement value.
#[derive(Debug)]
pub enum Replacement {
    /// Text that must be re-encoded by the calling codec (a custom
    /// handler's `str` result).
    Text(Vec<u32>),
    /// ASCII text from a built-in `backslashreplace`-style handler,
    /// re-encoded through the calling codec.
    Ascii(String),
    /// Raw bytes emitted verbatim (PEP 383 surrogateescape /
    /// surrogatepass — CPython writes these inline even when they break
    /// UTF-16/32 unit alignment).
    Bytes(Vec<u8>),
    /// Bytes from a *custom* handler; must keep the output unit-aligned.
    CustomBytes(Vec<u8>),
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

/// Encode with a per-code-point emitter: `emit` returns `true` if it encoded
/// the point, `false` to run the error handler. Handler `Text` replacements
/// are re-encoded through `emit` (a still-unencodable point raises).
/// `unit` is the codec's code-unit width: CPython's UTF-16/32 encoders
/// reject bytes replacements that aren't a whole number of units.
/// `ascii_repl`: the UTF encoders only accept *ASCII* str replacements
/// (CPython re-raises the original error otherwise); charmap-style codecs
/// re-encode the replacement through the map instead.
fn encode_loop_units(
    ctx: &mut EncCtx,
    reason: &str,
    unit: usize,
    ascii_repl: bool,
    mut emit: impl FnMut(u32, &mut Vec<u8>) -> bool,
) -> Result<Vec<u8>, RuntimeError> {
    let mut out: Vec<u8> = Vec::with_capacity(ctx.input.len());
    let mut i = 0usize;
    while i < ctx.input.len() {
        let cp = ctx.input[i];
        if emit(cp, &mut out) {
            i += 1;
            continue;
        }
        // Collect the full run of unencodable points (CPython batches them
        // into a single handler call).
        let start = i;
        let mut end = i + 1;
        while end < ctx.input.len() && {
            let mut probe = Vec::new();
            !emit_probe(&mut emit, ctx.input[end], &mut probe)
        } {
            end += 1;
        }
        let (rep, newpos) = ctx.on_error(start, end, reason)?;
        match rep {
            // Built-in byte replacements (surrogateescape/surrogatepass) are
            // emitted verbatim — CPython writes them inline even when they
            // break UTF-16/32 unit alignment.
            Replacement::Bytes(b) => out.extend(b),
            Replacement::CustomBytes(b) => {
                // A custom handler's bytes must keep the output unit-aligned.
                if unit > 1 && b.len() % unit != 0 {
                    return Err(ctx.strict(start, start + 1, reason));
                }
                out.extend(b)
            }
            // Built-in text replacements (backslashreplace/…) are pure ASCII
            // and get encoded through the codec itself (`\u` in UTF-16 is
            // 4 bytes, not 2).
            Replacement::Ascii(s) => {
                for c in s.chars() {
                    if !emit(c as u32, &mut out) {
                        return Err(ctx.strict(start, end, reason));
                    }
                }
            }
            Replacement::Text(cps) => {
                if ascii_repl && cps.iter().any(|&rc| rc >= 0x80) {
                    // CPython's UTF encoders only accept ASCII str
                    // replacements; anything else re-raises the original
                    // error at the handler position.
                    return Err(ctx.strict(start, start + 1, reason));
                }
                for &rc in &cps {
                    if !emit(rc, &mut out) {
                        return Err(ctx.strict(start, end, reason));
                    }
                }
            }
        }
        i = newpos;
    }
    Ok(out)
}

/// [`encode_loop_units`] for single-byte codecs (any replacement length,
/// replacements re-encoded through the codec itself).
fn encode_loop(
    ctx: &mut EncCtx,
    reason: &str,
    emit: impl FnMut(u32, &mut Vec<u8>) -> bool,
) -> Result<Vec<u8>, RuntimeError> {
    encode_loop_units(ctx, reason, 1, false, emit)
}

/// Probe whether `emit` accepts `cp` without keeping its output.
fn emit_probe(
    emit: &mut impl FnMut(u32, &mut Vec<u8>) -> bool,
    cp: u32,
    scratch: &mut Vec<u8>,
) -> bool {
    emit(cp, scratch)
}

pub fn utf8_encode(cps: &[u32], errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut ctx = EncCtx::new("utf-8", errors, cps);
    encode_loop_units(
        &mut ctx,
        "surrogates not allowed",
        1,
        true,
        |cp, out| match char::from_u32(cp) {
            Some(ch) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                true
            }
            None => false,
        },
    )
}

pub fn ascii_encode(cps: &[u32], errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut ctx = EncCtx::new("ascii", errors, cps);
    encode_loop(&mut ctx, "ordinal not in range(128)", |cp, out| {
        if cp < 0x80 {
            out.push(cp as u8);
            true
        } else {
            false
        }
    })
}

pub fn latin1_encode(cps: &[u32], errors: &str) -> Result<Vec<u8>, RuntimeError> {
    let mut ctx = EncCtx::new("latin-1", errors, cps);
    encode_loop(&mut ctx, "ordinal not in range(256)", |cp, out| {
        if cp < 0x100 {
            out.push(cp as u8);
            true
        } else {
            false
        }
    })
}

/// UTF-16 encode. `byteorder` < 0 LE, > 0 BE, 0 native-with-BOM.
pub fn utf16_encode(cps: &[u32], errors: &str, byteorder: i32) -> Result<Vec<u8>, RuntimeError> {
    let big = byteorder > 0;
    let name = if byteorder == 0 {
        "utf-16"
    } else if big {
        "utf-16-be"
    } else {
        "utf-16-le"
    };
    let mut ctx = EncCtx::new(name, errors, cps);
    let push16 = move |out: &mut Vec<u8>, u: u16| {
        if big {
            out.extend_from_slice(&u.to_be_bytes());
        } else {
            out.extend_from_slice(&u.to_le_bytes());
        }
    };
    let mut body = encode_loop_units(&mut ctx, "surrogates not allowed", 2, true, |cp, out| {
        if (0xD800..=0xDFFF).contains(&cp) {
            false
        } else if cp <= 0xFFFF {
            push16(out, cp as u16);
            true
        } else {
            let v = cp - 0x1_0000;
            push16(out, 0xD800 + (v >> 10) as u16);
            push16(out, 0xDC00 + (v & 0x3FF) as u16);
            true
        }
    })?;
    if byteorder == 0 {
        let mut with_bom = Vec::with_capacity(body.len() + 2);
        with_bom.extend_from_slice(&[0xFF, 0xFE]); // native (LE) BOM
        with_bom.append(&mut body);
        return Ok(with_bom);
    }
    Ok(body)
}

/// UTF-32 encode (same byteorder protocol).
pub fn utf32_encode(cps: &[u32], errors: &str, byteorder: i32) -> Result<Vec<u8>, RuntimeError> {
    let big = byteorder > 0;
    let name = if byteorder == 0 {
        "utf-32"
    } else if big {
        "utf-32-be"
    } else {
        "utf-32-le"
    };
    let mut ctx = EncCtx::new(name, errors, cps);
    let push32 = move |out: &mut Vec<u8>, u: u32| {
        if big {
            out.extend_from_slice(&u.to_be_bytes());
        } else {
            out.extend_from_slice(&u.to_le_bytes());
        }
    };
    let mut body = encode_loop_units(&mut ctx, "surrogates not allowed", 4, true, |cp, out| {
        if (0xD800..=0xDFFF).contains(&cp) {
            false
        } else {
            push32(out, cp);
            true
        }
    })?;
    if byteorder == 0 {
        let mut with_bom = Vec::with_capacity(body.len() + 4);
        with_bom.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x00]);
        with_bom.append(&mut body);
        return Ok(with_bom);
    }
    Ok(body)
}

/// Port of `_PyUnicode_EncodeUTF7` (conservative sets; `set O` and
/// whitespace are base64-encoded, matching `_codecs.utf_7_encode`).
pub fn utf7_encode(cps: &[u32], _errors: &str) -> Result<Vec<u8>, RuntimeError> {
    // Category table from CPython: 0 = Set D (direct), 1 = Set O,
    // 2 = whitespace, 3 = special.
    const CAT: [u8; 128] = [
        3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 3, 3, 2, 3, 3, //
        3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, //
        2, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 3, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, //
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 3, 1, 1, 1, //
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 3, 3,
    ];
    // `_codecs.utf_7_encode` calls with base64SetO=0, base64WhiteSpace=0,
    // i.e. Set O *and* whitespace are encoded directly = false/true?
    // CPython: directO = !base64SetO (true), directWS = !base64WhiteSpace
    // (true) — Set O and whitespace pass through directly.
    let encode_direct = |cp: u32| cp > 0 && cp < 128 && matches!(CAT[cp as usize], 0..=2);
    let is_b64 = |cp: u32| cp < 128 && is_base64(cp as u8);

    let mut out: Vec<u8> = Vec::with_capacity(cps.len() * 3);
    let mut in_shift = false;
    let mut base64bits: u32 = 0;
    let mut base64buffer: u64 = 0;

    let encode_char = |ch: u32, out: &mut Vec<u8>, base64bits: &mut u32, base64buffer: &mut u64| {
        let push_units = |u: u32, out: &mut Vec<u8>, bits: &mut u32, buf: &mut u64| {
            *bits += 16;
            *buf = (*buf << 16) | u64::from(u);
            while *bits >= 6 {
                out.push(B64_CHARS[((*buf >> (*bits - 6)) & 0x3F) as usize]);
                *bits -= 6;
            }
        };
        if ch >= 0x1_0000 {
            let v = ch - 0x1_0000;
            push_units(0xD800 + (v >> 10), out, base64bits, base64buffer);
            push_units(0xDC00 + (v & 0x3FF), out, base64bits, base64buffer);
        } else {
            push_units(ch, out, base64bits, base64buffer);
        }
    };

    for &ch in cps {
        if in_shift {
            if encode_direct(ch) {
                if base64bits > 0 {
                    out.push(B64_CHARS[((base64buffer << (6 - base64bits)) & 0x3F) as usize]);
                    base64buffer = 0;
                    base64bits = 0;
                }
                in_shift = false;
                if is_b64(ch) || ch == u32::from(b'-') {
                    out.push(b'-');
                }
                out.push(ch as u8);
            } else {
                encode_char(ch, &mut out, &mut base64bits, &mut base64buffer);
            }
        } else if ch == u32::from(b'+') {
            out.push(b'+');
            out.push(b'-');
        } else if encode_direct(ch) {
            out.push(ch as u8);
        } else {
            out.push(b'+');
            in_shift = true;
            encode_char(ch, &mut out, &mut base64bits, &mut base64buffer);
        }
    }
    if base64bits > 0 {
        out.push(B64_CHARS[((base64buffer << (6 - base64bits)) & 0x3F) as usize]);
    }
    if in_shift {
        out.push(b'-');
    }
    Ok(out)
}
