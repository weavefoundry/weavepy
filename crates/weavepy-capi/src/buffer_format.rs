//! Parser for PEP 3118 buffer-protocol format strings.
//!
//! Buffer-protocol exporters describe element layout via a string
//! borrowed from the `struct` module (`'B'` for unsigned byte, `'i'`
//! for `int`, `'<f4'` for little-endian 32-bit float, etc.). The
//! parser produced by [`parse`] reads exactly the subset CPython's
//! own runtime understands: a single optional byte-order prefix
//! (`'<' '>' '=' '@' '!'`), an optional repeat count, and a single
//! type code.
//!
//! The parser is deliberately lenient — unknown codes resolve to
//! [`FormatKind::Unknown`] with a defensive `itemsize` of 1 — so
//! callers handle the failure mode at use site rather than crash.
//!
//! ## Why a Rust-side parser
//!
//! - [`PyBuffer_SizeFromFormat`](crate::buffer::PyBuffer_SizeFromFormat)
//!   needs to convert a format string to a byte count, which is the
//!   product of repeat count and itemsize.
//! - The `_ndarray` integration fixture (and any future numpy bridge)
//!   reads the format off a `Py_buffer` to dispatch into typed loops
//!   without round-tripping through C `struct.unpack`.
//! - The bundled regression tests in
//!   `tests/regrtest/test_capi_buffer_format.rs` use this parser
//!   directly to assert the format strings emitted by built-in
//!   exporters round-trip correctly.

use std::os::raw::c_char;
use std::os::raw::c_int;

/// Decoded format string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatSpec {
    /// Byte order requested by the format prefix. Defaults to `Native`
    /// when no prefix is present.
    pub byte_order: ByteOrder,
    /// The element type the format encodes.
    pub kind: FormatKind,
    /// Repeat count if the format has a numeric prefix (`"3i"` →
    /// `count = 3`). Defaults to 1.
    pub count: usize,
    /// Element size in bytes for one occurrence (i.e. `count = 1`'s
    /// worth of data).
    pub itemsize: usize,
}

impl FormatSpec {
    /// Total byte size of the buffer described by this spec —
    /// `count * itemsize`.
    pub fn nbytes(&self) -> usize {
        self.count.saturating_mul(self.itemsize)
    }
}

/// Byte-order modifiers from the `struct` module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteOrder {
    /// `'@'` (default if absent): native byte order, native size,
    /// native alignment. Matches `Py_BUF_*` reflection of `int`,
    /// `long`, etc. on the host.
    Native,
    /// `'='`: native byte order, standard sizes, no alignment.
    NoAlign,
    /// `'<'`: little-endian, standard sizes.
    Little,
    /// `'>'`: big-endian, standard sizes.
    Big,
    /// `'!'`: network (big-endian).
    Network,
}

impl ByteOrder {
    pub fn is_little_endian(self) -> bool {
        matches!(self, Self::Little)
            || (matches!(self, Self::Native | Self::NoAlign) && cfg!(target_endian = "little"))
    }

    pub fn is_big_endian(self) -> bool {
        matches!(self, Self::Big | Self::Network)
            || (matches!(self, Self::Native | Self::NoAlign) && cfg!(target_endian = "big"))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ByteOrder::Native => "@",
            ByteOrder::NoAlign => "=",
            ByteOrder::Little => "<",
            ByteOrder::Big => ">",
            ByteOrder::Network => "!",
        }
    }
}

/// One supported element type. Sizes match the `struct` module's
/// "standard" sizing (always `[<>=!]` prefix) when the format
/// declares one explicitly; the native variants honour the host
/// platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatKind {
    /// `'?'` — bool (1 byte)
    Bool,
    /// `'b'` — signed char
    Int8,
    /// `'B'` — unsigned char
    UInt8,
    /// `'h'` — signed short
    Int16,
    /// `'H'` — unsigned short
    UInt16,
    /// `'i'` / `'l'` (when standard size): signed 32 bit
    Int32,
    /// `'I'` / `'L'`: unsigned 32 bit
    UInt32,
    /// `'q'`: signed 64 bit
    Int64,
    /// `'Q'`: unsigned 64 bit
    UInt64,
    /// `'n'` — `Py_ssize_t`
    SsizeT,
    /// `'N'` — `Py_size_t`
    SizeT,
    /// `'e'` — IEEE 754 binary16 (half-float)
    Float16,
    /// `'f'` — IEEE 754 binary32
    Float32,
    /// `'d'` / `'g'` — IEEE 754 binary64
    Float64,
    /// `'c'` — single-byte ASCII
    Char,
    /// `'s'` — fixed-length null-padded string
    String,
    /// `'p'` — Pascal string
    PascalString,
    /// `'P'` — `void *` (pointer-sized)
    Pointer,
    /// `'O'` — `PyObject *`
    PyObject,
    /// `'Z'` — Python complex (CPython 3.11+) — twin-double layout
    ComplexF64,
    /// `'F'` — twin-float complex
    ComplexF32,
    /// Unknown / unrecognised; itemsize falls back to 1 to keep
    /// downstream arithmetic safe.
    Unknown,
}

impl FormatKind {
    /// "Standard" itemsize as declared by the `struct` module's
    /// sizing rules (`'<'`/`'>'`/`'='`/`'!'`).
    pub fn standard_itemsize(self) -> usize {
        match self {
            FormatKind::Bool | FormatKind::Int8 | FormatKind::UInt8 | FormatKind::Char => 1,
            FormatKind::Int16 | FormatKind::UInt16 | FormatKind::Float16 => 2,
            FormatKind::Int32 | FormatKind::UInt32 | FormatKind::Float32 => 4,
            FormatKind::Int64 | FormatKind::UInt64 | FormatKind::Float64 => 8,
            FormatKind::ComplexF32 => 8,
            FormatKind::ComplexF64 => 16,
            FormatKind::SsizeT | FormatKind::SizeT | FormatKind::Pointer | FormatKind::PyObject => {
                std::mem::size_of::<usize>()
            }
            FormatKind::String | FormatKind::PascalString => 1,
            FormatKind::Unknown => 1,
        }
    }

    /// Itemsize when the format declared no byte-order prefix
    /// (i.e. native sizing). Differences from `standard_itemsize`
    /// are limited to the C-typed variants — `int` and `long` —
    /// because the others have stable widths.
    pub fn native_itemsize(self) -> usize {
        match self {
            FormatKind::Int32 => std::mem::size_of::<std::os::raw::c_int>(),
            FormatKind::UInt32 => std::mem::size_of::<std::os::raw::c_uint>(),
            FormatKind::Int64 => std::mem::size_of::<std::os::raw::c_longlong>(),
            FormatKind::UInt64 => std::mem::size_of::<std::os::raw::c_ulonglong>(),
            other => other.standard_itemsize(),
        }
    }

    /// Single character (bytewise) representation, matching CPython's
    /// preferred format-string encoding for the type.
    pub fn type_char(self) -> u8 {
        match self {
            FormatKind::Bool => b'?',
            FormatKind::Int8 => b'b',
            FormatKind::UInt8 => b'B',
            FormatKind::Int16 => b'h',
            FormatKind::UInt16 => b'H',
            FormatKind::Int32 => b'i',
            FormatKind::UInt32 => b'I',
            FormatKind::Int64 => b'q',
            FormatKind::UInt64 => b'Q',
            FormatKind::SsizeT => b'n',
            FormatKind::SizeT => b'N',
            FormatKind::Float16 => b'e',
            FormatKind::Float32 => b'f',
            FormatKind::Float64 => b'd',
            FormatKind::ComplexF32 => b'F',
            FormatKind::ComplexF64 => b'Z',
            FormatKind::Char => b'c',
            FormatKind::String => b's',
            FormatKind::PascalString => b'p',
            FormatKind::Pointer => b'P',
            FormatKind::PyObject => b'O',
            FormatKind::Unknown => b'?',
        }
    }
}

/// Parse a `struct`-style format string. Always succeeds — unknown
/// codes resolve to [`FormatKind::Unknown`] with a defensive
/// `itemsize` of 1.
///
/// The parser only consumes one type unit (the leading optional
/// prefix + count + code). Extra trailing bytes are silently
/// ignored, mirroring the way CPython's buffer protocol treats
/// `format = "B  "` as a synonym for `"B"`.
pub fn parse(s: &str) -> FormatSpec {
    let bytes = s.as_bytes();
    let (byte_order, mut idx) = match bytes.first().copied() {
        Some(b'@') => (ByteOrder::Native, 1),
        Some(b'=') => (ByteOrder::NoAlign, 1),
        Some(b'<') => (ByteOrder::Little, 1),
        Some(b'>') => (ByteOrder::Big, 1),
        Some(b'!') => (ByteOrder::Network, 1),
        _ => (ByteOrder::Native, 0),
    };

    // Repeat count.
    let mut count: usize = 0;
    let mut saw_digit = false;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        count = count * 10 + (bytes[idx] - b'0') as usize;
        idx += 1;
        saw_digit = true;
    }
    if !saw_digit {
        count = 1;
    }

    // Type code. If we see another optional digit suffix (e.g. `<f4`),
    // we treat the prefix as the type and the digits as a per-type
    // size selector — that's the numpy convention.
    let kind = bytes.get(idx).copied().unwrap_or(0);
    idx += 1;
    let mut size_suffix: Option<usize> = None;
    let mut tail_idx = idx;
    while tail_idx < bytes.len() && bytes[tail_idx].is_ascii_digit() {
        let v = (bytes[tail_idx] - b'0') as usize;
        size_suffix = Some(size_suffix.unwrap_or(0) * 10 + v);
        tail_idx += 1;
    }

    let mut format_kind = match kind {
        b'?' => FormatKind::Bool,
        b'b' => FormatKind::Int8,
        b'B' => FormatKind::UInt8,
        b'c' => FormatKind::Char,
        b'h' => FormatKind::Int16,
        b'H' => FormatKind::UInt16,
        b'i' => FormatKind::Int32,
        b'I' => FormatKind::UInt32,
        b'l' => FormatKind::Int32,
        b'L' => FormatKind::UInt32,
        // numpy convention: lowercase `u` is the unsigned-int family
        // (the type is sized through the suffix, e.g. `<u4` ↦ UInt32).
        // We start at the smallest variant and let numpy_resize widen.
        b'u' => FormatKind::UInt8,
        b'q' => FormatKind::Int64,
        b'Q' => FormatKind::UInt64,
        b'n' => FormatKind::SsizeT,
        b'N' => FormatKind::SizeT,
        b'e' => FormatKind::Float16,
        b'f' => FormatKind::Float32,
        b'd' | b'g' => FormatKind::Float64,
        b'F' => FormatKind::ComplexF32,
        b'Z' | b'D' => FormatKind::ComplexF64,
        b's' | b'a' => FormatKind::String,
        b'p' => FormatKind::PascalString,
        b'P' => FormatKind::Pointer,
        b'O' => FormatKind::PyObject,
        _ => FormatKind::Unknown,
    };

    // numpy size suffix override: `<f4` ↦ Float32, `<f8` ↦ Float64,
    // `<i4` ↦ Int32, `<i8` ↦ Int64, `<u4` ↦ UInt32 etc.
    if let Some(width) = size_suffix {
        format_kind = numpy_resize(format_kind, width).unwrap_or(format_kind);
    }

    let itemsize = match (byte_order, format_kind) {
        (ByteOrder::Native, k) => k.native_itemsize(),
        (_, k) => k.standard_itemsize(),
    };

    // For string-like formats the count *is* the itemsize.
    if matches!(format_kind, FormatKind::String) {
        return FormatSpec {
            byte_order,
            kind: format_kind,
            count: 1,
            itemsize: count.max(1),
        };
    }

    FormatSpec {
        byte_order,
        kind: format_kind,
        count,
        itemsize,
    }
}

/// numpy-style size suffix translation: `f4` ↦ float32, `f8` ↦
/// float64, `i2` ↦ int16, etc.
fn numpy_resize(k: FormatKind, width: usize) -> Option<FormatKind> {
    Some(match (k, width) {
        (FormatKind::Float16 | FormatKind::Float32 | FormatKind::Float64, 2) => FormatKind::Float16,
        (FormatKind::Float16 | FormatKind::Float32 | FormatKind::Float64, 4) => FormatKind::Float32,
        (FormatKind::Float16 | FormatKind::Float32 | FormatKind::Float64, 8) => FormatKind::Float64,
        (FormatKind::Int8 | FormatKind::Int16 | FormatKind::Int32 | FormatKind::Int64, 1) => {
            FormatKind::Int8
        }
        (FormatKind::Int8 | FormatKind::Int16 | FormatKind::Int32 | FormatKind::Int64, 2) => {
            FormatKind::Int16
        }
        (FormatKind::Int8 | FormatKind::Int16 | FormatKind::Int32 | FormatKind::Int64, 4) => {
            FormatKind::Int32
        }
        (FormatKind::Int8 | FormatKind::Int16 | FormatKind::Int32 | FormatKind::Int64, 8) => {
            FormatKind::Int64
        }
        (FormatKind::UInt8 | FormatKind::UInt16 | FormatKind::UInt32 | FormatKind::UInt64, 1) => {
            FormatKind::UInt8
        }
        (FormatKind::UInt8 | FormatKind::UInt16 | FormatKind::UInt32 | FormatKind::UInt64, 2) => {
            FormatKind::UInt16
        }
        (FormatKind::UInt8 | FormatKind::UInt16 | FormatKind::UInt32 | FormatKind::UInt64, 4) => {
            FormatKind::UInt32
        }
        (FormatKind::UInt8 | FormatKind::UInt16 | FormatKind::UInt32 | FormatKind::UInt64, 8) => {
            FormatKind::UInt64
        }
        _ => return None,
    })
}

/// `PyBuffer_SizeFromFormat(format)` — return the byte size of the
/// buffer described by `format`. Returns -1 on null input. Does not
/// raise; CPython's documentation states callers should treat
/// errors as "fall back to 1".
///
/// # Safety
///
/// `format` must be either null or a valid null-terminated C string.
pub unsafe fn size_from_format(format: *const c_char) -> isize {
    if format.is_null() {
        return -1;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(format) };
    let s = cstr.to_string_lossy();
    parse(&s).nbytes() as isize
}

/// Build a fresh format string for a kind+byte_order pair.
/// Returns a null-terminated C string allocated on the heap; the
/// caller owns the buffer.
pub fn format_string_for(kind: FormatKind, byte_order: ByteOrder) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    if !matches!(byte_order, ByteOrder::Native) {
        out.push(byte_order.as_str().as_bytes()[0]);
    }
    out.push(kind.type_char());
    out.push(0);
    out
}

/// `PyBuffer_HasFlag` helper — checks one of the requested-flags
/// bits. Both the requested mask and the comparison value are the
/// CPython-canonical numbers.
pub fn flag_set(requested: c_int, mask: c_int) -> bool {
    (requested & mask) == mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_codes() {
        let f = parse("B");
        assert_eq!(f.kind, FormatKind::UInt8);
        assert_eq!(f.byte_order, ByteOrder::Native);
        assert_eq!(f.count, 1);
        assert_eq!(f.itemsize, 1);

        let f = parse("d");
        assert_eq!(f.kind, FormatKind::Float64);
        assert_eq!(f.itemsize, 8);
    }

    #[test]
    fn parses_byte_order_prefix() {
        let f = parse("<f");
        assert_eq!(f.kind, FormatKind::Float32);
        assert_eq!(f.byte_order, ByteOrder::Little);
        assert_eq!(f.itemsize, 4);

        let f = parse(">i");
        assert_eq!(f.kind, FormatKind::Int32);
        assert_eq!(f.byte_order, ByteOrder::Big);
        assert_eq!(f.itemsize, 4);

        let f = parse("!Q");
        assert_eq!(f.kind, FormatKind::UInt64);
        assert_eq!(f.byte_order, ByteOrder::Network);
        assert_eq!(f.itemsize, 8);
    }

    #[test]
    fn parses_repeat_count() {
        let f = parse("3i");
        assert_eq!(f.count, 3);
        assert_eq!(f.kind, FormatKind::Int32);
        assert_eq!(f.itemsize, 4);
        assert_eq!(f.nbytes(), 12);
    }

    #[test]
    fn parses_numpy_size_suffix() {
        let f = parse("<f4");
        assert_eq!(f.kind, FormatKind::Float32);
        assert_eq!(f.itemsize, 4);

        let f = parse(">f8");
        assert_eq!(f.kind, FormatKind::Float64);
        assert_eq!(f.itemsize, 8);

        let f = parse("<i2");
        assert_eq!(f.kind, FormatKind::Int16);
        assert_eq!(f.itemsize, 2);

        let f = parse("<u4");
        assert_eq!(f.kind, FormatKind::UInt32);
        assert_eq!(f.itemsize, 4);
    }

    #[test]
    fn fixed_length_strings() {
        let f = parse("32s");
        assert_eq!(f.kind, FormatKind::String);
        assert_eq!(f.itemsize, 32);
        assert_eq!(f.count, 1);
        assert_eq!(f.nbytes(), 32);
    }

    #[test]
    fn unknown_codes_fall_back_safely() {
        let f = parse("Q3");
        // 'Q' is uint64, count default 1, but a trailing '3' suffix
        // narrows to UInt64 since 8 matches.
        assert_eq!(f.kind, FormatKind::UInt64);
        assert_eq!(f.itemsize, 8);

        let f = parse("§");
        assert_eq!(f.kind, FormatKind::Unknown);
        assert_eq!(f.itemsize, 1);
    }

    #[test]
    fn format_string_for_round_trips() {
        let s = format_string_for(FormatKind::Float64, ByteOrder::Little);
        assert_eq!(&s[..s.len() - 1], b"<d");
    }

    #[test]
    fn flag_set_helper() {
        assert!(flag_set(0xff, 0x10));
        assert!(!flag_set(0x0f, 0x10));
    }
}
