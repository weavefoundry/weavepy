//! `_struct` — binary packing / unpacking (RFC 0019).
//!
//! Implements the full CPython `struct` module surface:
//!
//! * Format strings with the `<>=!@` byte-order/alignment prefix and
//!   the per-character type table (`bBhHiIlLqQfdcs?eExNn` plus the
//!   pointer character `P`).
//! * `pack`, `unpack`, `pack_into`, `unpack_from`, `iter_unpack`,
//!   `calcsize`.
//! * `Struct` class (precompiled format) with `pack`,
//!   `unpack`, `pack_into`, `unpack_from`, `iter_unpack`,
//!   `format`, `size`.
//! * `error` class (alias of `struct.error`, an `Exception`
//!   subclass).
//!
//! Frozen `python/struct.py` re-exports this surface as the
//! public `struct` module so user code can `from struct import
//! pack`. The frozen wrapper also exposes the `Struct.__init__`
//! glue that calls `_struct.compile(...)` and stashes the result
//! on the instance.

use crate::sync::Rc;
use crate::sync::RefCell;

use byteorder::{BigEndian, ByteOrder, LittleEndian, NativeEndian};

use crate::error::{overflow_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// Upper bound on a computed struct size, mirroring CPython's
/// `PY_SSIZE_T_MAX` guard in `_struct` (`prepare_s`). Beyond this we
/// raise `struct.error: total struct size too long` rather than letting
/// the repeat-count arithmetic overflow and panic the `Vec` allocator.
const MAX_STRUCT_SIZE: usize = isize::MAX as usize;

/// Format-string byte-order prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    /// `@` — native order, native size, native alignment.
    Native,
    /// `=` — native order, standard size, no alignment.
    Standard,
    /// `<` — little-endian, standard size, no alignment.
    Little,
    /// `>` / `!` — big-endian, standard size, no alignment.
    Big,
}

/// One field in a parsed format string.
#[derive(Debug, Clone, Copy)]
struct Field {
    /// CPython format character (e.g. `b`, `H`, `s`).
    code: char,
    /// Repeat count. For `s`/`p` this is a string length; for the
    /// other codes it's a multiplier.
    count: usize,
}

/// Unpack one element's worth of `fmt` from `buf` (which must be
/// exactly `calcsize(fmt)` bytes). Used by memoryview's rich
/// comparison, which unpacks elements per each view's full
/// struct-syntax format and compares the *values* (CPython
/// `memory_richcompare` → `struct_get_unpacker`, RFC 0066 WS1).
pub(crate) fn unpack_element(fmt: &str, buf: &[u8]) -> Result<Vec<Object>, RuntimeError> {
    let cf = CompiledFormat::parse(fmt)?;
    if buf.len() != cf.size {
        return Err(struct_error(format!(
            "unpack requires a buffer of {} bytes",
            cf.size
        )));
    }
    cf.unpack(buf)
}

/// A pre-compiled format string ready for repeated pack/unpack.
#[derive(Debug, Clone)]
struct CompiledFormat {
    endian: Endian,
    fields: Vec<Field>,
    size: usize,
}

impl CompiledFormat {
    /// Parse a CPython-shaped format string.
    fn parse(fmt: &str) -> Result<Self, RuntimeError> {
        // CPython treats the format as a C string, so an embedded NUL
        // terminates it early and is reported up front
        // (test_struct.test_issue35714), rather than falling through to
        // the generic "bad char" diagnostic.
        if fmt.contains('\0') {
            return Err(struct_error("embedded null character"));
        }
        let mut chars = fmt.chars().peekable();
        let endian = match chars.peek() {
            Some('@') => {
                chars.next();
                Endian::Native
            }
            Some('=') => {
                chars.next();
                Endian::Standard
            }
            Some('<') => {
                chars.next();
                Endian::Little
            }
            Some('>') | Some('!') => {
                chars.next();
                Endian::Big
            }
            _ => Endian::Native,
        };
        let mut fields = Vec::new();
        let mut size = 0usize;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_whitespace() {
                chars.next();
                continue;
            }
            // Optional repeat count.
            let mut n: usize = 0;
            let mut had_count = false;
            while let Some(&d) = chars.peek() {
                if let Some(digit) = d.to_digit(10) {
                    n = n
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(digit as usize))
                        .ok_or_else(|| value_error("repeat count overflow"))?;
                    chars.next();
                    had_count = true;
                } else {
                    break;
                }
            }
            let code = chars
                .next()
                .ok_or_else(|| value_error("repeat count without format code"))?;
            if !had_count {
                n = 1;
            }
            // Treat 'x' (pad byte) and the other CPython codes
            // uniformly. Sanity-check the code itself.
            let unit = element_size(code, endian)?;
            // For 's' / 'p' the count is the byte count of the string;
            // each field is a single value but consumes `n` bytes.
            // Use checked arithmetic so a pathological repeat count
            // (e.g. `struct.calcsize('999999999999s')`) raises CPython's
            // `struct.error: total struct size too long` instead of
            // overflowing and panicking the `Vec` allocator.
            let bytes = unit
                .checked_mul(n)
                .filter(|b| *b <= MAX_STRUCT_SIZE)
                .ok_or_else(|| struct_error("total struct size too long"))?;
            // Native alignment: pad to alignment if @ mode.
            if endian == Endian::Native {
                let align = native_align(code);
                if align > 1 {
                    let pad = (align - (size % align)) % align;
                    size += pad;
                    if pad > 0 {
                        fields.push(Field {
                            code: 'x',
                            count: pad,
                        });
                    }
                }
            }
            fields.push(Field { code, count: n });
            size = size
                .checked_add(bytes)
                .filter(|s| *s <= MAX_STRUCT_SIZE)
                .ok_or_else(|| struct_error("total struct size too long"))?;
        }
        Ok(Self {
            endian,
            fields,
            size,
        })
    }

    fn pack(&self, values: &[Object]) -> Result<Vec<u8>, RuntimeError> {
        let needed_args: usize = self
            .fields
            .iter()
            .map(|f| match f.code {
                'x' => 0,
                's' | 'p' => 1,
                _ => f.count,
            })
            .sum();
        if values.len() != needed_args {
            return Err(struct_error(format!(
                "pack expected {} items for packing (got {})",
                needed_args,
                values.len()
            )));
        }
        // Cap the up-front reservation: `self.size` is already bounded by
        // `MAX_STRUCT_SIZE`, but a multi-gigabyte format shouldn't pre-
        // allocate everything at once — let the buffer grow as we write.
        let mut out = Vec::with_capacity(self.size.min(1 << 20));
        let mut idx = 0usize;
        for f in &self.fields {
            match f.code {
                'x' => {
                    out.extend(std::iter::repeat_n(0u8, f.count));
                }
                's' => {
                    let v = &values[idx];
                    idx += 1;
                    // The data's first `count` bytes, zero-padded to `count`,
                    // written straight from the operand.
                    let mut put = |data: &[u8]| {
                        let take = data.len().min(f.count);
                        out.extend_from_slice(&data[..take]);
                        out.resize(out.len() + (f.count - take), 0);
                    };
                    match v {
                        Object::Bytes(b) => put(b),
                        Object::ByteArray(b) => put(&b.borrow()),
                        Object::Str(s) => put(s.as_bytes()),
                        _ => {
                            return Err(struct_error(
                                "argument for 's' must be a bytes-like or str".to_owned(),
                            ))
                        }
                    }
                }
                'p' => {
                    // Pascal-style: first byte is length, up to count-1 bytes follow.
                    let v = &values[idx];
                    idx += 1;
                    let data = match v {
                        Object::Bytes(b) => b.to_vec(),
                        Object::ByteArray(b) => b.borrow().clone(),
                        Object::Str(s) => s.as_bytes().to_vec(),
                        _ => {
                            return Err(struct_error(
                                "argument for 'p' must be a bytes-like or str".to_owned(),
                            ))
                        }
                    };
                    let mut buf = vec![0u8; f.count];
                    if f.count > 0 {
                        // CPython copies up to `count - 1` data bytes, but the
                        // leading length byte saturates at 255 (the most a
                        // single byte can encode). For e.g. `1000p` of 1000
                        // bytes the buffer holds 999 data bytes yet the length
                        // prefix reads 255 (test_struct.test_p_code).
                        let copy = data.len().min(f.count - 1);
                        buf[0] = copy.min(255) as u8;
                        if copy > 0 {
                            buf[1..=copy].copy_from_slice(&data[..copy]);
                        }
                    }
                    out.extend_from_slice(&buf);
                }
                code => {
                    for _ in 0..f.count {
                        let v = &values[idx];
                        idx += 1;
                        encode_one(code, self.endian, v, &mut out)?;
                    }
                }
            }
        }
        Ok(out)
    }

    fn unpack(&self, buf: &[u8]) -> Result<Vec<Object>, RuntimeError> {
        if buf.len() != self.size {
            return Err(struct_error(format!(
                "unpack requires a buffer of {} bytes",
                self.size
            )));
        }
        self.unpack_from_offset(buf, 0).map(|(v, _)| v)
    }

    fn iter_unpack(&self, buf: &[u8]) -> Result<Vec<Vec<Object>>, RuntimeError> {
        if !buf.len().is_multiple_of(self.size) {
            return Err(struct_error(format!(
                "iterative unpacking requires a buffer of a multiple of {} bytes",
                self.size
            )));
        }
        let mut out = Vec::with_capacity(buf.len() / self.size.max(1));
        let mut offset = 0;
        while offset < buf.len() {
            let (vals, consumed) = self.unpack_from_offset(buf, offset)?;
            out.push(vals);
            offset += consumed;
        }
        Ok(out)
    }

    fn unpack_from_offset(
        &self,
        buf: &[u8],
        start: usize,
    ) -> Result<(Vec<Object>, usize), RuntimeError> {
        let values = self
            .fields
            .iter()
            .map(|f| match f.code {
                'x' => 0,
                's' | 'p' => 1,
                _ => f.count,
            })
            .sum();
        let mut out = Vec::with_capacity(values);
        let mut pos = start;
        for f in &self.fields {
            match f.code {
                'x' => {
                    pos += f.count;
                }
                's' => {
                    let bytes = &buf[pos..pos + f.count];
                    out.push(Object::new_bytes(bytes));
                    pos += f.count;
                }
                'p' => {
                    if f.count == 0 {
                        out.push(Object::new_bytes(Vec::new()));
                        continue;
                    }
                    let len = buf[pos] as usize;
                    let take = len.min(f.count - 1);
                    let bytes = &buf[pos + 1..pos + 1 + take];
                    out.push(Object::new_bytes(bytes));
                    pos += f.count;
                }
                code => {
                    for _ in 0..f.count {
                        let (val, consumed) = decode_one(code, self.endian, &buf[pos..])?;
                        pos += consumed;
                        out.push(val);
                    }
                }
            }
        }
        Ok((out, pos - start))
    }
}

/// Element size in bytes for a single repeat unit of `code`.
/// Returns an error for unknown codes.
fn element_size(code: char, endian: Endian) -> Result<usize, RuntimeError> {
    Ok(match code {
        'x' | 'b' | 'B' | 'c' | 's' | 'p' | '?' => 1,
        'h' | 'H' | 'e' => 2,
        'i' | 'I' | 'f' => 4,
        // C `long` is the one code whose native size differs from its
        // standard size: 8 bytes on an LP64 target (native/`@` mode), 4
        // bytes in the standard/explicit-endian modes. This must match
        // `sizeof(c_long)` so `ctypes._check_size(c_long)` agrees.
        'l' | 'L' => match endian {
            Endian::Native => std::mem::size_of::<libc::c_long>(),
            _ => 4,
        },
        // 3.14: `F` is a C `float complex` (two binary32), `D` a
        // `double complex` (two binary64), in every mode.
        'q' | 'Q' | 'd' | 'F' => 8,
        'D' => 16,
        'n' | 'N' => match endian {
            Endian::Native => std::mem::size_of::<isize>(),
            // In standard / explicit-endian modes `n`/`N` simply aren't
            // recognised; CPython reports the same "bad char" diagnostic as
            // for any other unknown code (test_struct.test_nN_code).
            _ => return Err(struct_error(format!("bad char in struct format: '{code}'"))),
        },
        'P' => match endian {
            Endian::Native => std::mem::size_of::<usize>(),
            _ => return Err(struct_error(format!("bad char in struct format: '{code}'"))),
        },
        _ => return Err(struct_error(format!("bad char in struct format: '{code}'"))),
    })
}

fn native_align(code: char) -> usize {
    match code {
        'x' | 'b' | 'B' | 'c' | 's' | 'p' | '?' => 1,
        'h' | 'H' | 'e' => 2,
        'i' | 'I' | 'f' | 'F' => 4,
        'l' | 'L' => std::mem::align_of::<libc::c_long>(),
        'q' | 'Q' | 'd' | 'D' => 8,
        'n' | 'N' => std::mem::size_of::<isize>(),
        'P' => std::mem::size_of::<usize>(),
        _ => 1,
    }
}

#[allow(clippy::cast_lossless)]
fn encode_one(
    code: char,
    endian: Endian,
    value: &Object,
    out: &mut Vec<u8>,
) -> Result<(), RuntimeError> {
    macro_rules! write_int {
        ($t:ty, $set:ident, $signed:expr) => {{
            let n = require_int(value)?;
            let lo = <$t>::MIN as i128;
            let hi = <$t>::MAX as i128;
            if !($signed) {
                if n < 0 {
                    return Err(struct_error(format!(
                        "argument out of range for '{code}'"
                    )));
                }
            }
            if n < lo || n > hi {
                return Err(struct_error(format!(
                    "argument out of range for '{code}'"
                )));
            }
            let mut buf = [0u8; std::mem::size_of::<$t>()];
            match endian {
                Endian::Native => NativeEndian::$set(&mut buf, n as $t),
                Endian::Standard => LittleEndian::$set(&mut buf, n as $t), // standard = LE on most platforms;
                                                                            // we override below for explicit endians
                Endian::Little => LittleEndian::$set(&mut buf, n as $t),
                Endian::Big => BigEndian::$set(&mut buf, n as $t),
            };
            out.extend_from_slice(&buf);
            Ok(())
        }};
    }
    match code {
        'b' => {
            let n = require_int(value)?;
            if !(-128..=127).contains(&n) {
                return Err(struct_error("argument out of range for 'b'".to_owned()));
            }
            out.push(n as i8 as u8);
            Ok(())
        }
        'B' => {
            let n = require_int(value)?;
            if !(0..=255).contains(&n) {
                return Err(struct_error("argument out of range for 'B'".to_owned()));
            }
            out.push(n as u8);
            Ok(())
        }
        'c' => {
            // CPython: only a length-1 bytes object packs — a str (even of
            // length 1) or bytearray is a struct.error
            // (test_memoryview_struct_module asserts struct and memoryview
            // agree on refusing 'a').
            let bytes = match value {
                Object::Bytes(b) if b.len() == 1 => b.to_vec(),
                _ => {
                    return Err(struct_error(
                        "char format requires a bytes object of length 1".to_owned(),
                    ))
                }
            };
            out.push(bytes[0]);
            Ok(())
        }
        '?' => {
            let v = u8::from(value.is_truthy());
            out.push(v);
            Ok(())
        }
        'h' => write_int!(i16, write_i16, true),
        'H' => write_int!(u16, write_u16, false),
        'i' => write_int!(i32, write_i32, true),
        'I' => write_int!(u32, write_u32, false),
        // Native `long`/`unsigned long` are 8-byte on LP64; standard modes
        // keep them 4-byte (see `element_size`).
        'l' => {
            if matches!(endian, Endian::Native) {
                write_int!(i64, write_i64, true)
            } else {
                write_int!(i32, write_i32, true)
            }
        }
        'L' => {
            if matches!(endian, Endian::Native) {
                write_int!(u64, write_u64, false)
            } else {
                write_int!(u32, write_u32, false)
            }
        }
        'q' => write_int!(i64, write_i64, true),
        'Q' => write_int!(u64, write_u64, false),
        'f' => {
            // Strip the WeavePy NaN identity tag — CPython packs the
            // object's canonical `ob_fval` bits (see `untag_nan`).
            let f = crate::object::untag_nan(require_float(value)?);
            // A finite double whose magnitude rounds above `FLT_MAX`
            // overflows binary32. CPython's `_PyFloat_Pack4` reports this
            // as `OverflowError` (not `struct.error`), so the frozen
            // wrapper lets it propagate (test_struct.test_705836).
            let f32v = f as f32;
            if f.is_finite() && f32v.is_infinite() {
                return Err(overflow_error("float too large to pack with f format"));
            }
            let mut buf = [0u8; 4];
            match endian {
                Endian::Native => NativeEndian::write_f32(&mut buf, f32v),
                Endian::Standard | Endian::Little => LittleEndian::write_f32(&mut buf, f32v),
                Endian::Big => BigEndian::write_f32(&mut buf, f32v),
            }
            out.extend_from_slice(&buf);
            Ok(())
        }
        'd' => {
            let f = crate::object::untag_nan(require_float(value)?);
            let mut buf = [0u8; 8];
            match endian {
                Endian::Native => NativeEndian::write_f64(&mut buf, f),
                Endian::Standard | Endian::Little => LittleEndian::write_f64(&mut buf, f),
                Endian::Big => BigEndian::write_f64(&mut buf, f),
            }
            out.extend_from_slice(&buf);
            Ok(())
        }
        'F' => {
            // `np_float_complex`: real then imaginary, each packed like
            // `f` (so a component above FLT_MAX is the same OverflowError).
            let (re, im) = require_complex(value)?;
            let mut buf = [0u8; 4];
            for part in [re, im] {
                let f32v = part as f32;
                if part.is_finite() && f32v.is_infinite() {
                    return Err(overflow_error("float too large to pack with f format"));
                }
                match endian {
                    Endian::Native => NativeEndian::write_f32(&mut buf, f32v),
                    Endian::Standard | Endian::Little => LittleEndian::write_f32(&mut buf, f32v),
                    Endian::Big => BigEndian::write_f32(&mut buf, f32v),
                }
                out.extend_from_slice(&buf);
            }
            Ok(())
        }
        'D' => {
            let (re, im) = require_complex(value)?;
            let mut buf = [0u8; 8];
            for part in [re, im] {
                match endian {
                    Endian::Native => NativeEndian::write_f64(&mut buf, part),
                    Endian::Standard | Endian::Little => LittleEndian::write_f64(&mut buf, part),
                    Endian::Big => BigEndian::write_f64(&mut buf, part),
                }
                out.extend_from_slice(&buf);
            }
            Ok(())
        }
        'e' => {
            // Half-precision IEEE 754, converted from the double with
            // round-half-to-even (CPython `_PyFloat_Pack2`), not via an
            // intermediate `f32` truncation.
            let f = crate::object::untag_nan(require_float(value)?);
            let half = f64_to_half(f)?;
            let mut buf = [0u8; 2];
            match endian {
                Endian::Native => NativeEndian::write_u16(&mut buf, half),
                Endian::Standard | Endian::Little => LittleEndian::write_u16(&mut buf, half),
                Endian::Big => BigEndian::write_u16(&mut buf, half),
            }
            out.extend_from_slice(&buf);
            Ok(())
        }
        'n' => {
            let n = require_int(value)?;
            let v =
                isize::try_from(n).map_err(|_| struct_error("argument out of range for 'n'"))?;
            let mut buf = vec![0u8; std::mem::size_of::<isize>()];
            NativeEndian::write_int(&mut buf, v as i64, std::mem::size_of::<isize>());
            out.extend_from_slice(&buf);
            Ok(())
        }
        'N' | 'P' => {
            let n = require_int(value)?;
            let v = u64::try_from(n).map_err(|_| struct_error("argument out of range"))?;
            let mut buf = vec![0u8; std::mem::size_of::<usize>()];
            NativeEndian::write_uint(&mut buf, v, std::mem::size_of::<usize>());
            out.extend_from_slice(&buf);
            Ok(())
        }
        _ => Err(struct_error(format!("bad char in struct format: '{code}'"))),
    }
}

fn decode_one(code: char, endian: Endian, buf: &[u8]) -> Result<(Object, usize), RuntimeError> {
    let n = element_size(code, endian)?;
    if buf.len() < n {
        return Err(struct_error("buffer too short"));
    }
    let val = match code {
        'b' => Object::Int(i64::from(buf[0] as i8)),
        'B' => Object::Int(i64::from(buf[0])),
        'c' => Object::new_bytes(vec![buf[0]]),
        '?' => Object::Bool(buf[0] != 0),
        'h' => Object::Int(i64::from(read_i16(endian, &buf[..2]))),
        'H' => Object::Int(i64::from(read_u16(endian, &buf[..2]))),
        'i' => Object::Int(i64::from(read_i32(endian, &buf[..4]))),
        'I' => Object::Int(i64::from(read_u32(endian, &buf[..4]))),
        // Native `long` is 8-byte on LP64; standard modes keep it 4-byte.
        'l' => {
            if matches!(endian, Endian::Native) {
                Object::Int(read_i64(endian, &buf[..8]))
            } else {
                Object::Int(i64::from(read_i32(endian, &buf[..4])))
            }
        }
        'L' => {
            if matches!(endian, Endian::Native) {
                let v = read_u64(endian, &buf[..8]);
                if i64::try_from(v).is_ok() {
                    Object::Int(v as i64)
                } else {
                    Object::int_from_bigint(num_bigint::BigInt::from(v))
                }
            } else {
                Object::Int(i64::from(read_u32(endian, &buf[..4])))
            }
        }
        'q' => Object::Int(read_i64(endian, &buf[..8])),
        'Q' => {
            let v = read_u64(endian, &buf[..8]);
            if i64::try_from(v).is_ok() {
                Object::Int(v as i64)
            } else {
                Object::int_from_bigint(num_bigint::BigInt::from(v))
            }
        }
        // A canonical NaN read from bytes gets a fresh identity (CPython
        // allocates a new object); an exotic payload is kept verbatim so
        // `pack(unpack(b)) == b` round-trips — see `tag_unpacked_nan`.
        'f' => Object::Float(crate::object::tag_unpacked_nan(f64::from(read_f32(
            endian,
            &buf[..4],
        )))),
        'd' => Object::Float(crate::object::tag_unpacked_nan(read_f64(endian, &buf[..8]))),
        'F' => Object::Complex(Rc::new(crate::object::PyComplex::new(
            f64::from(read_f32(endian, &buf[..4])),
            f64::from(read_f32(endian, &buf[4..8])),
        ))),
        'D' => Object::Complex(Rc::new(crate::object::PyComplex::new(
            read_f64(endian, &buf[..8]),
            read_f64(endian, &buf[8..16]),
        ))),
        'e' => Object::Float(crate::object::tag_unpacked_nan(f64::from(half_to_f32(
            read_u16(endian, &buf[..2]),
        )))),
        'n' => {
            let v = NativeEndian::read_int(
                &buf[..std::mem::size_of::<isize>()],
                std::mem::size_of::<isize>(),
            );
            Object::Int(v)
        }
        'N' | 'P' => {
            let v = NativeEndian::read_uint(
                &buf[..std::mem::size_of::<usize>()],
                std::mem::size_of::<usize>(),
            );
            if i64::try_from(v).is_ok() {
                Object::Int(v as i64)
            } else {
                Object::int_from_bigint(num_bigint::BigInt::from(v))
            }
        }
        _ => return Err(struct_error(format!("bad char in struct format: '{code}'"))),
    };
    Ok((val, n))
}

fn read_i16(endian: Endian, b: &[u8]) -> i16 {
    match endian {
        Endian::Native => NativeEndian::read_i16(b),
        Endian::Standard | Endian::Little => LittleEndian::read_i16(b),
        Endian::Big => BigEndian::read_i16(b),
    }
}
fn read_u16(endian: Endian, b: &[u8]) -> u16 {
    match endian {
        Endian::Native => NativeEndian::read_u16(b),
        Endian::Standard | Endian::Little => LittleEndian::read_u16(b),
        Endian::Big => BigEndian::read_u16(b),
    }
}
fn read_i32(endian: Endian, b: &[u8]) -> i32 {
    match endian {
        Endian::Native => NativeEndian::read_i32(b),
        Endian::Standard | Endian::Little => LittleEndian::read_i32(b),
        Endian::Big => BigEndian::read_i32(b),
    }
}
fn read_u32(endian: Endian, b: &[u8]) -> u32 {
    match endian {
        Endian::Native => NativeEndian::read_u32(b),
        Endian::Standard | Endian::Little => LittleEndian::read_u32(b),
        Endian::Big => BigEndian::read_u32(b),
    }
}
fn read_i64(endian: Endian, b: &[u8]) -> i64 {
    match endian {
        Endian::Native => NativeEndian::read_i64(b),
        Endian::Standard | Endian::Little => LittleEndian::read_i64(b),
        Endian::Big => BigEndian::read_i64(b),
    }
}
fn read_u64(endian: Endian, b: &[u8]) -> u64 {
    match endian {
        Endian::Native => NativeEndian::read_u64(b),
        Endian::Standard | Endian::Little => LittleEndian::read_u64(b),
        Endian::Big => BigEndian::read_u64(b),
    }
}
fn read_f32(endian: Endian, b: &[u8]) -> f32 {
    match endian {
        Endian::Native => NativeEndian::read_f32(b),
        Endian::Standard | Endian::Little => LittleEndian::read_f32(b),
        Endian::Big => BigEndian::read_f32(b),
    }
}
fn read_f64(endian: Endian, b: &[u8]) -> f64 {
    match endian {
        Endian::Native => NativeEndian::read_f64(b),
        Endian::Standard | Endian::Little => LittleEndian::read_f64(b),
        Endian::Big => BigEndian::read_f64(b),
    }
}

/// `frexp`: decompose a finite, non-NaN `x` into `(m, e)` with
/// `x == m * 2**e` and `0.5 <= |m| < 1` (or `m == 0` for `x == 0`).
/// std doesn't ship `frexp`, so we do it by exponent-field surgery.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || x.is_nan() || x.is_infinite() {
        return (x, 0);
    }
    let exp_field = ((x.to_bits() >> 52) & 0x7ff) as i32;
    if exp_field == 0 {
        // Subnormal: scale into the normal range first, then correct `e`.
        let scaled = x * f64::from_bits(0x43f0_0000_0000_0000); // * 2**64
        let exp_s = ((scaled.to_bits() >> 52) & 0x7ff) as i32 - 64;
        let m_bits = (scaled.to_bits() & !(0x7ffu64 << 52)) | (1022u64 << 52);
        (f64::from_bits(m_bits), exp_s - 1022)
    } else {
        let m_bits = (x.to_bits() & !(0x7ffu64 << 52)) | (1022u64 << 52);
        (f64::from_bits(m_bits), exp_field - 1022)
    }
}

#[inline]
fn ldexp(f: f64, n: i32) -> f64 {
    f * 2f64.powi(n)
}

/// Port of CPython's `_PyFloat_Pack2` (`Objects/floatobject.c`):
/// convert a double to an IEEE 754 binary16 bit pattern with
/// round-half-to-even, returning the value in host order. Raises
/// `OverflowError` on overflow, exactly like CPython
/// (test_struct.test_705836 / test_half_float assert `OverflowError`,
/// which is *not* a `struct.error`).
fn f64_to_half(x: f64) -> Result<u16, RuntimeError> {
    let sign: u16;
    let mut e: i32;
    let mut bits: u16;
    if x == 0.0 {
        sign = u16::from(x.is_sign_negative());
        e = 0;
        bits = 0;
    } else if x.is_infinite() {
        sign = u16::from(x < 0.0);
        e = 0x1f;
        bits = 0;
    } else if x.is_nan() {
        sign = u16::from(x.is_sign_negative());
        e = 0x1f;
        bits = 512;
    } else {
        sign = u16::from(x < 0.0);
        let ax = x.abs();
        let (mut f, fe) = frexp(ax);
        e = fe;
        // Normalize f to [1.0, 2.0).
        f *= 2.0;
        e -= 1;
        if e >= 16 {
            return Err(overflow_error("float too large to pack with e format"));
        } else if e < -25 {
            // |x| < 2**-25 — underflow to (signed) zero.
            f = 0.0;
            e = 0;
        } else if e < -14 {
            // Gradual underflow (subnormal half).
            f = ldexp(f, 14 + e);
            e = 0;
        } else {
            e += 15;
            f -= 1.0; // strip the implicit leading 1
        }
        f *= 1024.0; // 2**10
        bits = f as u16; // truncating cast
                         // Round half to even.
        let frac = f - f64::from(bits);
        if frac > 0.5 || (frac == 0.5 && (bits & 1) == 1) {
            bits += 1;
            if bits == 1024 {
                // Carry rippled out of the 10-bit mantissa.
                bits = 0;
                e += 1;
                if e == 31 {
                    return Err(overflow_error("float too large to pack with e format"));
                }
            }
        }
    }
    Ok(bits | ((e as u16) << 10) | (sign << 15))
}

fn half_to_f32(half: u16) -> f32 {
    let sign = u32::from(half & 0x8000);
    let exp = i32::from((half >> 10) & 0x1F);
    let mantissa = u32::from(half & 0x03FF);
    let bits: u32 = if exp == 0 {
        if mantissa == 0 {
            sign << 16
        } else {
            // Subnormal — normalise.
            let mut e = 1;
            let mut m = mantissa;
            while m & 0x0400 == 0 {
                m <<= 1;
                e -= 1;
            }
            let new_exp = (e + 127 - 15) as u32;
            (sign << 16) | (new_exp << 23) | ((m & 0x03FF) << 13)
        }
    } else if exp == 0x1F {
        let new_mantissa = mantissa << 13;
        (sign << 16) | (0xFFu32 << 23) | new_mantissa
    } else {
        let new_exp = (exp + 127 - 15) as u32;
        (sign << 16) | (new_exp << 23) | (mantissa << 13)
    };
    f32::from_bits(bits)
}

fn require_int(v: &Object) -> Result<i128, RuntimeError> {
    match v {
        Object::Int(i) => Ok(i128::from(*i)),
        Object::Long(b) => num_traits::ToPrimitive::to_i128(&**b)
            .ok_or_else(|| struct_error("int too large to pack")),
        Object::Bool(b) => Ok(i128::from(i64::from(*b))),
        // CPython's `_struct` runs a non-int through `PyNumber_Index`
        // (`__index__`) for the integer formats, so a numpy integer scalar
        // (`np.int64`, as produced by pandas `Tick` offset arithmetic) or any
        // `__index__`-bearing object packs correctly. A float or an object
        // with no `__index__` still errors below.
        _ => match crate::builtins::try_coerce_index_i64(v) {
            Some(r) => r.map(i128::from),
            None => Err(struct_error(format!(
                "required argument is not an integer (got '{}')",
                v.type_name()
            ))),
        },
    }
}

/// Coerce a `struct.pack` float-format argument (`f`/`d`/`e`) to `f64`.
/// CPython's `_struct` runs these through `PyFloat_AsDouble`, honouring
/// `__float__`/`nb_float` (then `__index__`), so a numpy float scalar
/// (`np.float64`) or any real-number-like object packs correctly instead
/// of being rejected as "not a float".
/// `PyComplex_AsCComplex` for the `F`/`D` codes: a complex passes
/// through, a real coerces with a zero imaginary part (the frozen
/// wrapper has already run `__complex__`/`__float__`/`__index__`).
fn require_complex(v: &Object) -> Result<(f64, f64), RuntimeError> {
    if let Object::Complex(c) = v {
        return Ok((
            crate::object::untag_nan(c.real),
            crate::object::untag_nan(c.imag),
        ));
    }
    match crate::builtins::coerce_f64_opt(v)? {
        Some(f) => Ok((crate::object::untag_nan(f), 0.0)),
        None => Err(struct_error("required argument is not a complex")),
    }
}

fn require_float(v: &Object) -> Result<f64, RuntimeError> {
    match crate::builtins::coerce_f64_opt(v)? {
        Some(f) => Ok(f),
        None => Err(struct_error("required argument is not a float")),
    }
}

fn struct_error(msg: impl Into<String>) -> RuntimeError {
    // We expose a `struct.error` type via the frozen Python wrapper;
    // here we produce a `ValueError` and the wrapper rewraps it. The
    // CPython convention is `struct.error` is itself an Exception,
    // not a `ValueError` subclass — when we run in pure-Rust mode,
    // value_error is the closest signal.
    value_error(msg.into())
}

// ---------- public API surface ----------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_struct"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Binary data packing/unpacking (RFC 0019 core)."),
        );
        register(&mut d, "calcsize", b_calcsize);
        register(&mut d, "_value_codes", b_value_codes);
        register(&mut d, "pack", b_pack);
        register(&mut d, "_pack_plain", b_pack_plain);
        register(&mut d, "_native_pack", b_native_pack);
        register(&mut d, "_native_unpack", b_native_unpack);
        register(&mut d, "_native_unpack_from", b_native_unpack_from);
        register(&mut d, "_native_pack_into", b_native_pack_into);
        register(&mut d, "_clearcache", b_clearcache);
        register(&mut d, "unpack", b_unpack);
        register(&mut d, "pack_into", b_pack_into);
        register(&mut d, "unpack_from", b_unpack_from);
        register(&mut d, "iter_unpack", b_iter_unpack);
        let install = Rc::new(BuiltinFn {
            name: "_install_struct",
            binds_instance: false,
            call: Box::new(install_struct),
            call_kw: None,
        });
        d.insert(
            DictKey(Object::from_static("_install_struct")),
            Object::Builtin(install),
        );
        register(&mut d, "_array_methods", array_methods);
        register(&mut d, "_unpack_iterator_methods", unpack_iterator_methods);
        register(&mut d, "_all_exact", all_exact);
    }
    Rc::new(PyModule {
        name: "_struct".to_owned(),
        filename: None,
        dict,
    })
}

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    let bf = Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    });
    // The core reads and writes plain values only (the Python wrapper
    // coerces first): no Python runs, so the dispatch loop calls it in
    // place.
    crate::leaf_builtins::register(&bf);
    d.insert(DictKey(Object::from_static(name)), Object::Builtin(bf));
}

thread_local! {
    /// Compiled formats by their text (CPython's `_struct` cache, flushed
    /// by `_clearcache` and bounded as CPython bounds its own).
    static FORMATS: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<CompiledFormat>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// How many compiled formats [`FORMATS`] keeps before starting over.
const MAX_FORMATS: usize = 100;

/// `fmt` compiled, from the cache when it was compiled before.
fn compiled(fmt: &str) -> Result<std::rc::Rc<CompiledFormat>, RuntimeError> {
    if let Some(cf) = FORMATS.with(|c| c.borrow().get(fmt).cloned()) {
        return Ok(cf);
    }
    let cf = std::rc::Rc::new(CompiledFormat::parse(fmt)?);
    FORMATS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= MAX_FORMATS {
            c.clear();
        }
        c.insert(fmt.to_owned(), cf.clone());
    });
    Ok(cf)
}

/// `_clearcache()`: flush the compiled formats.
fn b_clearcache(_args: &[Object]) -> Result<Object, RuntimeError> {
    FORMATS.with(|c| c.borrow_mut().clear());
    BY_STRING.with(|c| c.borrow_mut().clear());
    Ok(Object::None)
}

/// The address of the exact `struct.Struct` class [`install_struct`]
/// last served (its native methods keep it alive).
static STRUCT_CLASS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// Compiled formats by the identity of a `Struct`'s format string
    /// (each held here, so an address names one string), most recent
    /// first: a `Struct` hands its methods the same string every call.
    static BY_STRING: crate::gil::ReleaseUnderGil<std::cell::RefCell<Vec<(crate::shared_value::SharedStr, std::rc::Rc<CompiledFormat>)>>> = const { crate::gil::ReleaseUnderGil::new(std::cell::RefCell::new(Vec::new())) };
}

/// How many format strings [`BY_STRING`] remembers.
const BY_STRING_MAX: usize = 8;

/// The compiled format of a `Struct`'s format string `fmt`.
fn compiled_for(fmt: &crate::shared_value::SharedStr) -> Option<std::rc::Rc<CompiledFormat>> {
    let addr = crate::shared_value::SharedStr::addr(fmt);
    let hit = BY_STRING.with(|c| {
        c.try_borrow().ok().and_then(|c| {
            c.iter()
                .find(|(s, _)| crate::shared_value::SharedStr::addr(s) == addr)
                .map(|(_, cf)| cf.clone())
        })
    });
    if hit.is_some() {
        return hit;
    }
    let cf = compiled(fmt).ok()?;
    BY_STRING.with(|c| {
        if let Ok(mut c) = c.try_borrow_mut() {
            c.truncate(BY_STRING_MAX - 1);
            c.insert(0, (fmt.clone(), cf.clone()));
        }
    });
    Some(cf)
}

/// The compiled format of `recv` when it is an exact, initialized
/// `struct.Struct` whose `_fmt` is a plain `str` in its own dictionary
/// (anything else is the Python method's to serve).
fn struct_format(recv: &Object) -> Option<std::rc::Rc<CompiledFormat>> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    let class = std::ptr::from_ref(inst.cls_raw()) as usize;
    if class != STRUCT_CLASS.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    match inst.attr_get_str("_fmt")? {
        Object::Str(fmt) => compiled_for(&fmt),
        _ => None,
    }
}

/// `Struct.pack(self, *values)` over plain values (what `_pack_plain`
/// packs), or `None` for the Python method, which also owns every error.
fn struct_pack_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (recv, values) = args.split_first()?;
    let plain = values.iter().all(|v| {
        matches!(
            v,
            Object::Int(_)
                | Object::Long(_)
                | Object::Bool(_)
                | Object::Float(_)
                | Object::Complex(_)
                | Object::Str(_)
                | Object::Bytes(_)
                | Object::ByteArray(_)
        )
    });
    if !plain {
        return None;
    }
    let cf = struct_format(recv)?;
    let bytes = cf.pack(values).ok()?;
    Some(Ok(Object::new_bytes(bytes)))
}

/// `Struct.unpack(self, buffer)` of a `bytes` buffer, or `None` for the
/// Python method, which also owns every error.
fn struct_unpack_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, Object::Bytes(buf)] = args else {
        return None;
    };
    let cf = struct_format(recv)?;
    let vals = cf.unpack(buf).ok()?;
    Some(Ok(Object::new_tuple(vals)))
}

pub(crate) type Fast = fn(&[Object]) -> Option<Result<Object, RuntimeError>>;

/// A native method `name` for `cls`: it serves what `fast` takes and
/// calls the Python method `orig` (the one it replaces) for everything
/// else. With `keywords` it takes keyword arguments (for `orig`); without,
/// it is positional only, as a special method's slot is (the subscript
/// lanes take only such methods).
pub(crate) fn fast_method(
    cls: &Rc<crate::types::TypeObject>,
    name: &'static str,
    fast: Fast,
    orig: Object,
    keywords: bool,
) -> Object {
    // (The native keeps the class alive: its address stays its own.)
    let class = cls.clone();
    let fallback = move |a: &[Object], kw: &[(String, Object)]| {
        let _ = &class;
        let f = orig.clone();
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| type_error("no active interpreter"))?;
        // SAFETY: published by the enclosing VM frame on this thread.
        unsafe { &mut *ptr }.call_object(f, a, kw)
    };
    let fallback = Rc::new(fallback);
    let fallback_kw = fallback.clone();
    let b = Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |a: &[Object]| match fast(a) {
            Some(r) => r,
            None => fallback(a, &[]),
        }),
        call_kw: keywords.then(|| -> KwCall {
            Box::new(move |a: &[Object], kw: &[(String, Object)]| {
                if kw.is_empty() {
                    if let Some(r) = fast(a) {
                        return r;
                    }
                }
                fallback_kw(a, kw)
            })
        }),
    });
    crate::leaf_builtins::register_fast(&b, fast);
    Object::Builtin(b)
}

type KwCall =
    Box<dyn Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError> + Send + Sync>;

/// Put a [`fast_method`] in `cls`'s dictionary for each `(name, fast,
/// original)`.
fn install_fast_methods<const N: usize>(
    cls: &Rc<crate::types::TypeObject>,
    methods: [(&'static str, Fast, Object); N],
) {
    for (name, fast, orig) in methods {
        let method = fast_method(cls, name, fast, orig, true);
        cls.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static(name)), method);
    }
    cls.bump_attr_version();
}

// ---------------------------------------------------------------------
// `array.array` item access. The class is Python (`array_mod.py`, over a
// `bytearray` of packed items); its item methods are served here for the
// plain cases, as CPython's C type serves them, and fall back to the
// Python methods for everything else (other index and value types, every
// error).

/// An `array.array` instance's typecode and buffer.
fn array_parts(recv: &Object) -> Option<(u8, Rc<RefCell<Vec<u8>>>)> {
    with_array(recv, |typecode, buf| Some((typecode, buf.clone())))
}

/// The slot layouts of the `array` class and its iterator class (their
/// addresses), set when `_array_methods` serves them: an instance laid out
/// over one of these has its slots at known places.
static ARRAY_LAYOUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ARRAY_ITER_LAYOUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static UNPACK_ITER_LAYOUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `inst`'s slot values when it is laid out over the layout `known` names.
fn known_slots<'a>(
    inst: &crate::types::PyInstance,
    slots: &'a crate::types::SlotStorage,
    known: &std::sync::atomic::AtomicUsize,
) -> Option<&'a [Object]> {
    let layout = inst.cls_raw().own_slot_layout()?;
    if crate::shared_value::SharedSlice::word(layout)
        != known.load(std::sync::atomic::Ordering::Relaxed)
    {
        return None;
    }
    slots.values_for_layout(layout)
}

/// `f` on an `array.array` instance's typecode and buffer, the slots read
/// in place.
fn with_array<R>(
    recv: &Object,
    f: impl FnOnce(u8, &Rc<RefCell<Vec<u8>>>) -> Option<R>,
) -> Option<R> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    let slots = inst.slots.try_borrow().ok()?;
    let (code, buf) = match known_slots(inst, &slots, &ARRAY_LAYOUT) {
        Some(values) => (values.first()?, values.get(3)?),
        None => (
            slots.get_hinted(0, "_typecode")?,
            slots.get_hinted(3, "_buf")?,
        ),
    };
    match (code, buf) {
        (Object::Str(s), Object::ByteArray(b)) if s.len() == 1 => f(s.as_bytes()[0], b),
        _ => None,
    }
}

/// The size in bytes of a numeric typecode's items (the unicode arrays,
/// whose items are characters, are the Python methods').
fn array_item_size(typecode: u8) -> Option<usize> {
    Some(match typecode {
        b'b' | b'B' => 1,
        b'h' | b'H' => 2,
        b'i' | b'I' | b'f' => 4,
        b'l' | b'L' => std::mem::size_of::<std::ffi::c_long>(),
        b'q' | b'Q' | b'd' => 8,
        _ => return None,
    })
}

/// The item packed in `raw` (its typecode's size, native order), when it
/// is a machine int or a float.
fn array_item_read(typecode: u8, raw: &[u8]) -> Option<Object> {
    let signed = |raw: &[u8]| match raw.len() {
        4 => Some(i64::from(i32::from_ne_bytes(raw.try_into().ok()?))),
        8 => Some(i64::from_ne_bytes(raw.try_into().ok()?)),
        _ => None,
    };
    let unsigned = |raw: &[u8]| match raw.len() {
        4 => Some(i64::from(u32::from_ne_bytes(raw.try_into().ok()?))),
        // (One past the machine range is a big int: the Python method's.)
        8 => i64::try_from(u64::from_ne_bytes(raw.try_into().ok()?)).ok(),
        _ => None,
    };
    Some(match typecode {
        b'b' => Object::Int(i64::from(raw[0] as i8)),
        b'B' => Object::Int(i64::from(raw[0])),
        b'h' => Object::Int(i64::from(i16::from_ne_bytes(raw.try_into().ok()?))),
        b'H' => Object::Int(i64::from(u16::from_ne_bytes(raw.try_into().ok()?))),
        b'i' | b'l' | b'q' => Object::Int(signed(raw)?),
        b'I' | b'L' | b'Q' => Object::Int(unsigned(raw)?),
        b'f' => Object::Float(f64::from(f32::from_ne_bytes(raw.try_into().ok()?))),
        b'd' => Object::Float(f64::from_ne_bytes(raw.try_into().ok()?)),
        _ => return None,
    })
}

/// `value` packed as an item of `typecode` into `out`: its size, for a
/// plain `int` in the item's range or a `float` the item holds (a `float`
/// too large for an `'f'` item, like every other value, is the Python
/// method's, which converts or refuses it as CPython's setters do).
fn array_item_write(typecode: u8, value: &Object, out: &mut [u8; 8]) -> Option<usize> {
    let size = array_item_size(typecode)?;
    match (typecode, value) {
        (b'f', Object::Float(v)) => {
            let narrow = *v as f32;
            if narrow.is_infinite() && v.is_finite() {
                return None;
            }
            out[..4].copy_from_slice(&narrow.to_ne_bytes());
        }
        (b'd', Object::Float(v)) => out.copy_from_slice(&v.to_ne_bytes()),
        (b'b' | b'h' | b'i' | b'l' | b'q', Object::Int(v)) => {
            let bits = 8 * size as u32;
            if bits < 64 && !(-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(v) {
                return None;
            }
            out[..size].copy_from_slice(low_bytes(&v.to_ne_bytes(), size));
        }
        (b'B' | b'H' | b'I' | b'L' | b'Q', Object::Int(v)) => {
            let bits = 8 * size as u32;
            if *v < 0 || (bits < 64 && *v >= (1i64 << bits)) {
                return None;
            }
            out[..size].copy_from_slice(low_bytes(&v.to_ne_bytes(), size));
        }
        _ => return None,
    }
    Some(size)
}

/// The `size` low-order bytes of a native-order 64-bit value, in native
/// order.
fn low_bytes(bytes: &[u8; 8], size: usize) -> &[u8] {
    if cfg!(target_endian = "big") {
        &bytes[8 - size..]
    } else {
        &bytes[..size]
    }
}

/// The class `_array_methods` served last (its address): slices of its own
/// instances are built natively.
static ARRAY_CLASS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The byte range `sl` names in an array of `size`-byte items holding
/// `len` bytes, for a slice with plain bounds and a step of one.
fn array_byte_run(sl: &crate::object::PySlice, len: usize, size: usize) -> Option<(usize, usize)> {
    if ![&sl.start, &sl.stop, &sl.step]
        .iter()
        .all(|v| matches!(v, Object::None | Object::Int(_)))
    {
        return None;
    }
    let (start, stop, step, _) = crate::adjust_slice((len / size) as i64, sl).ok()?;
    if step != 1 {
        return None;
    }
    let lo = start as usize * size;
    Some((lo, (stop as usize * size).max(lo)))
}

/// The seven slot values of an instance of the `array` class itself
/// (`_typecode`, `_fmt`, `_itemsize`, `_buf`, `_kind`, `_lo`, `_hi`), read
/// in place when it is laid out over the class's layout with all of them
/// set.
fn array_values<'a>(
    inst: &crate::types::PyInstance,
    slots: &'a crate::types::SlotStorage,
) -> Option<&'a [Object; 7]> {
    if std::ptr::from_ref(inst.cls_raw()) as usize
        != ARRAY_CLASS.load(std::sync::atomic::Ordering::Relaxed)
    {
        return None;
    }
    let values: &[Object; 7] = known_slots(inst, slots, &ARRAY_LAYOUT)?.try_into().ok()?;
    (!values.iter().any(|v| matches!(v, Object::Unbound))).then_some(values)
}

/// A new instance of the `array` class with `values`' fields (see
/// [`array_values`]) over the bytes `run`, laid out as `_make_array` lays
/// one out (its slots written at once, in the class's layout).
fn array_over(
    inst: &crate::types::PyInstance,
    values: &[Object; 7],
    run: Object,
) -> Option<Object> {
    let layout = inst.cls_raw().own_slot_layout()?;
    let out = [
        values[0].clone(),
        values[1].clone(),
        values[2].clone(),
        run,
        values[4].clone(),
        values[5].clone(),
        values[6].clone(),
    ];
    Some(Object::Instance(Rc::new(
        crate::types::PyInstance::with_slots(
            inst.cls(),
            crate::types::SlotStorage::from_layout_array(layout.clone(), out),
        ),
    )))
}

/// The numeric typecode and buffer among [`array_values`]' fields.
fn array_code_buf(values: &[Object; 7]) -> Option<(u8, &Rc<RefCell<Vec<u8>>>)> {
    match (&values[0], &values[3]) {
        (Object::Str(code), Object::ByteArray(buf)) => match code.as_bytes() {
            [typecode] if array_item_size(*typecode).is_some() => Some((*typecode, buf)),
            _ => None,
        },
        _ => None,
    }
}

/// `a[i:j]` of an instance of the `array` class itself: a new array over
/// a copy of the run's bytes.
fn array_slice_fast(recv: &Object, sl: &crate::object::PySlice) -> Option<Object> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    let slots = inst.slots.try_borrow().ok()?;
    let values = array_values(inst, &slots)?;
    let (typecode, buf) = array_code_buf(values)?;
    let size = array_item_size(typecode)?;
    let run = {
        let buf = buf.try_borrow().ok()?;
        let (lo, hi) = array_byte_run(sl, buf.len(), size)?;
        Object::new_bytearray(&buf[lo..hi])
    };
    array_over(inst, values, run)
}

/// `a.__copy__()` (`copy.copy(a)`) of an instance of the `array` class
/// itself with a numeric typecode: a new array over a copy of its bytes.
fn array_copy_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(inst)] = args else {
        return None;
    };
    let slots = inst.slots.try_borrow().ok()?;
    let values = array_values(inst, &slots)?;
    let (_, buf) = array_code_buf(values)?;
    let run = Object::new_bytearray(buf.try_borrow().ok()?.as_slice());
    array_over(inst, values, run).map(Ok)
}

/// `a + b` of two instances of the `array` class itself with one numeric
/// typecode: a new array over both runs of bytes. (Anything else is the
/// Python method's, which raises for another typecode or type.)
fn array_add_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(a), Object::Instance(b)] = args else {
        return None;
    };
    let (sa, sb) = (a.slots.try_borrow().ok()?, b.slots.try_borrow().ok()?);
    let (va, vb) = (array_values(a, &sa)?, array_values(b, &sb)?);
    let ((ca, ba), (cb, bb)) = (array_code_buf(va)?, array_code_buf(vb)?);
    if ca != cb {
        return None;
    }
    let run = {
        let (x, y) = (ba.try_borrow().ok()?, bb.try_borrow().ok()?);
        let mut joined = Vec::with_capacity(x.len() + y.len());
        joined.extend_from_slice(&x);
        joined.extend_from_slice(&y);
        Object::new_bytearray(joined)
    };
    array_over(a, va, run).map(Ok)
}

/// The items of an instance of the `array` class itself (whose `__iter__`
/// is its own), as iterating it would give them, for the containers that
/// collect an iterable (`list(a)`, `tuple(a)`, `sorted(a)`).
pub(crate) fn array_items(recv: &Object) -> Option<Vec<Object>> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    if std::ptr::from_ref(inst.cls_raw()) as usize
        != ARRAY_CLASS.load(std::sync::atomic::Ordering::Relaxed)
    {
        return None;
    }
    // (Its own `__iter__`, which nothing has replaced.)
    match inst.cls_raw().lookup("__iter__") {
        Some(Object::Function(f)) if f.code().qualname == "array.__iter__" => {}
        _ => return None,
    }
    with_array(recv, |typecode, buf| {
        let size = array_item_size(typecode)?;
        let buf = buf.try_borrow().ok()?;
        buf.chunks_exact(size)
            .map(|raw| array_item_read(typecode, raw))
            .collect()
    })
}

/// `a[i]` for a plain index in range, and `a[i:j]` (see
/// [`array_slice_fast`]).
fn array_getitem_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    if let [recv, Object::Slice(sl)] = args {
        return array_slice_fast(recv, sl).map(Ok);
    }
    let [recv, Object::Int(index)] = args else {
        return None;
    };
    let (typecode, buf) = array_parts(recv)?;
    let size = array_item_size(typecode)?;
    let buf = buf.try_borrow().ok()?;
    let n = (buf.len() / size) as i64;
    let at = if *index < 0 { *index + n } else { *index };
    if !(0..n).contains(&at) {
        return None;
    }
    let off = at as usize * size;
    array_item_read(typecode, &buf[off..off + size]).map(Ok)
}

/// `a[i:j] = b` for another array of the same typecode: the run's bytes
/// replaced by `b`'s, in place when the lengths match, and otherwise only
/// while no view of `a` is exported.
fn array_store_run_fast(recv: &Object, sl: &crate::object::PySlice, value: &Object) -> Option<()> {
    let (typecode, buf) = array_parts(recv)?;
    let (other_code, other) = array_parts(value)?;
    if typecode != other_code || Rc::ptr_eq(&buf, &other) {
        return None;
    }
    let size = array_item_size(typecode)?;
    let source = other.try_borrow().ok()?;
    let mut target = buf.try_borrow_mut().ok()?;
    let (lo, hi) = array_byte_run(sl, target.len(), size)?;
    if hi - lo == source.len() {
        target[lo..hi].copy_from_slice(&source);
    } else {
        // (The export registry doesn't read the buffer: checking under the
        // borrow is safe.)
        if crate::object::bytearray_is_exported(&buf) {
            return None;
        }
        target.splice(lo..hi, source.iter().copied());
    }
    Some(())
}

/// `a[i] = v` for a plain index in range and a plain value the item
/// holds: written where it goes. And `a[i:j] = b` (see
/// [`array_store_run_fast`]).
fn array_setitem_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    if let [recv, Object::Slice(sl), value @ Object::Instance(_)] = args {
        return array_store_run_fast(recv, sl, value).map(|()| Ok(Object::None));
    }
    let [recv, Object::Int(index), value] = args else {
        return None;
    };
    let (typecode, buf) = array_parts(recv)?;
    let mut packed = [0u8; 8];
    let size = array_item_write(typecode, value, &mut packed)?;
    let mut buf = buf.try_borrow_mut().ok()?;
    let n = (buf.len() / size) as i64;
    let at = if *index < 0 { *index + n } else { *index };
    if !(0..n).contains(&at) {
        return None;
    }
    let off = at as usize * size;
    buf[off..off + size].copy_from_slice(&packed[..size]);
    Some(Ok(Object::None))
}

/// `a.append(v)` for a plain value the item holds, while no view of the
/// array is exported (a resize under one is refused by the Python method).
fn array_append_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, value] = args else {
        return None;
    };
    let (typecode, buf) = array_parts(recv)?;
    let mut packed = [0u8; 8];
    let size = array_item_write(typecode, value, &mut packed)?;
    if crate::object::bytearray_is_exported(&buf) {
        return None;
    }
    buf.try_borrow_mut()
        .ok()?
        .extend_from_slice(&packed[..size]);
    Some(Ok(Object::None))
}

/// `len(a)`.
fn array_len_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv] = args else {
        return None;
    };
    let (typecode, buf) = array_parts(recv)?;
    let size = array_item_size(typecode)?;
    let n = buf.try_borrow().ok()?.len() / size;
    Some(Ok(Object::Int(n as i64)))
}

/// An array iterator's next item while it has one (its end, and an
/// iterator already finished, are the Python method's).
fn array_next_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(it)] = args else {
        return None;
    };
    let read = |array: &Object, index: i64| {
        with_array(array, |typecode, buf| {
            let size = array_item_size(typecode)?;
            let buf = buf.try_borrow().ok()?;
            let off = usize::try_from(index).ok()?.checked_mul(size)?;
            array_item_read(typecode, buf.get(off..off + size)?)
        })
    };
    let mut slots = it.slots.try_borrow_mut().ok()?;
    // (Laid out as the class declares them: read and advanced in place.)
    let layout = it.cls_raw().own_slot_layout()?;
    if crate::shared_value::SharedSlice::word(layout)
        == ARRAY_ITER_LAYOUT.load(std::sync::atomic::Ordering::Relaxed)
    {
        let values = slots.values_for_layout_mut(layout)?;
        let [array, index @ Object::Int(_), ..] = values else {
            return None;
        };
        let Object::Int(i) = *index else {
            return None;
        };
        let item = read(array, i)?;
        *index = Object::Int(i + 1);
        return Some(Ok(item));
    }
    let Object::Int(i) = *slots.get_hinted(1, "_index")? else {
        return None;
    };
    let item = read(slots.get_hinted(0, "_ao")?, i)?;
    *slots.get_hinted_mut(1, "_index")? = Object::Int(i + 1);
    Some(Ok(item))
}

/// An `unpack_iterator`'s next tuple while it has one (its end is the
/// Python method's): the format's fields read at the iterator's offset,
/// which then moves on by the format's size.
fn unpack_iter_next_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(it)] = args else {
        return None;
    };
    // (The class's own layout, `_fmt, _buffer, _size, _offset, _len`, is
    // read at its places; anything else by name.)
    let (fmt, buffer, offset) = {
        let slots = it.slots.try_borrow().ok()?;
        let laid = known_slots(it, &slots, &UNPACK_ITER_LAYOUT);
        let (fmt, offset, len) = match laid {
            Some([f, _, _, o, l]) => (f, o, l),
            _ => (
                slots.get_hinted(0, "_fmt")?,
                slots.get_hinted(3, "_offset")?,
                slots.get_hinted(4, "_len")?,
            ),
        };
        let (Object::Str(fmt), Object::Int(offset), Object::Int(len)) = (fmt, offset, len) else {
            return None;
        };
        if offset >= len {
            return None;
        }
        let buffer = match laid {
            Some([_, b, ..]) => b,
            _ => slots.get_hinted(1, "_buffer")?,
        };
        (fmt.clone(), buffer.clone(), *offset)
    };
    let cf = compiled_for(&fmt)?;
    let read = |buf: &[u8]| {
        let off = usize::try_from(offset).ok()?;
        if off.checked_add(cf.size)? > buf.len() {
            return None;
        }
        cf.unpack_from_offset(buf, off).ok().map(|(vals, _)| vals)
    };
    let vals = match &buffer {
        Object::Bytes(b) => read(b)?,
        Object::ByteArray(b) => read(&b.try_borrow().ok()?)?,
        Object::MemoryView(mv) if !mv.released.get() && mv.is_c_contiguous() => {
            mv.with_bytes(read)?
        }
        _ => return None,
    };
    let next = Object::Int(offset + cf.size as i64);
    let mut slots = it.slots.try_borrow_mut().ok()?;
    let laid = it.cls_raw().own_slot_layout().filter(|layout| {
        crate::shared_value::SharedSlice::word(layout)
            == UNPACK_ITER_LAYOUT.load(std::sync::atomic::Ordering::Relaxed)
    });
    let at = match laid {
        Some(layout) => slots.values_for_layout_mut(layout)?.get_mut(3)?,
        None => slots.get_hinted_mut(3, "_offset")?,
    };
    *at = next;
    drop(slots);
    Some(Ok(Object::new_tuple(vals)))
}

/// `_all_exact(items, cls)`: whether every item of the list is exactly of
/// the builtin class `cls` (`int` or `float`: `array.fromlist` packs such
/// a list in one call). `False` for any other class or argument.
fn all_exact(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::List(items), Object::Type(cls)] = args else {
        return Ok(Object::Bool(false));
    };
    let bt = crate::builtin_types::builtin_types();
    let items = items.borrow();
    let all = if Rc::ptr_eq(cls, &bt.int_) {
        items
            .iter()
            .all(|v| matches!(v, Object::Int(_) | Object::Long(_)))
    } else if Rc::ptr_eq(cls, &bt.float_) {
        items.iter().all(|v| matches!(v, Object::Float(_)))
    } else {
        false
    };
    Ok(Object::Bool(all))
}

/// `_unpack_iterator_methods(unpack_iterator)`: the iterator's native
/// step, as `(class, name, method)` triples for the module to assign.
fn unpack_iterator_methods(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error(
            "_unpack_iterator_methods() expects the iterator class",
        ));
    };
    let orig = cls
        .dict
        .borrow()
        .get(&crate::object::StrKey("__next__"))
        .cloned()
        .ok_or_else(|| type_error("_unpack_iterator_methods(): no __next__ to replace"))?;
    if let Some(layout) = cls.own_slot_layout() {
        UNPACK_ITER_LAYOUT.store(
            crate::shared_value::SharedSlice::word(layout),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    Ok(Object::new_list(vec![Object::new_tuple(vec![
        Object::Type(cls.clone()),
        Object::from_static("__next__"),
        fast_method(cls, "__next__", unpack_iter_next_fast, orig, false),
    ])]))
}

/// `_array_methods(array, array_iterator)`: the native item methods of
/// the `array` class and its iterator's native step, as `(class, name,
/// method)` triples for the module to assign (a class attribute
/// assignment keeps the special-method caches right).
fn array_methods(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), Object::Type(iter_cls)] = args else {
        return Err(type_error(
            "_array_methods() expects the array class and its iterator's",
        ));
    };
    let table: [(&Rc<crate::types::TypeObject>, &'static str, Fast); 7] = [
        (cls, "__getitem__", array_getitem_fast),
        (cls, "__add__", array_add_fast),
        (cls, "__copy__", array_copy_fast),
        (cls, "__setitem__", array_setitem_fast),
        (cls, "__len__", array_len_fast),
        (cls, "append", array_append_fast),
        (iter_cls, "__next__", array_next_fast),
    ];
    ARRAY_CLASS.store(
        Rc::as_ptr(cls) as usize,
        std::sync::atomic::Ordering::Relaxed,
    );
    for (owner, known) in [(cls, &ARRAY_LAYOUT), (iter_cls, &ARRAY_ITER_LAYOUT)] {
        if let Some(layout) = owner.own_slot_layout() {
            known.store(
                crate::shared_value::SharedSlice::word(layout),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
    let mut out = Vec::with_capacity(table.len());
    for (owner, name, fast) in table {
        let orig = owner
            .dict
            .borrow()
            .get(&crate::object::StrKey(name))
            .cloned()
            .ok_or_else(|| type_error(format!("_array_methods(): no {name} to replace")))?;
        out.push(Object::new_tuple(vec![
            Object::Type(owner.clone()),
            Object::from_static(name),
            fast_method(owner, name, fast, orig, false),
        ]));
    }
    Ok(Object::new_list(out))
}

/// `_install_struct(Struct)`: serve the exact class's `pack` and
/// `unpack` natively (CPython's `Struct` is a C type), each falling back
/// to the Python method it replaces for every other shape.
fn install_struct(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls)] = args else {
        return Err(type_error("_install_struct() expects the Struct class"));
    };
    let own = |name: &str| cls.dict.borrow().get(&crate::object::StrKey(name)).cloned();
    let (Some(pack), Some(unpack)) = (own("pack"), own("unpack")) else {
        return Err(type_error("_install_struct(): Struct lacks pack/unpack"));
    };
    install_fast_methods(
        cls,
        [
            ("pack", struct_pack_fast as Fast, pack),
            ("unpack", struct_unpack_fast as Fast, unpack),
        ],
    );
    cls.bump_attr_version();
    STRUCT_CLASS.store(
        Rc::as_ptr(cls) as usize,
        std::sync::atomic::Ordering::Relaxed,
    );
    Ok(Object::None)
}

/// `pack(fmt, *values)` when every value is one the packer reads directly
/// (a plain int, float, complex, bool, str, bytes or bytearray), else
/// `None` for the wrapper's coercing path, having done nothing.
fn b_pack_plain(args: &[Object]) -> Result<Object, RuntimeError> {
    if !args.get(1..).unwrap_or(&[]).iter().all(|v| {
        matches!(
            v,
            Object::Int(_)
                | Object::Long(_)
                | Object::Bool(_)
                | Object::Float(_)
                | Object::Complex(_)
                | Object::Str(_)
                | Object::Bytes(_)
                | Object::ByteArray(_)
        )
    }) {
        return Ok(Object::None);
    }
    b_pack(args)
}

fn fmt_arg(args: &[Object], idx: usize) -> Result<String, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(Object::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        _ => Err(type_error("format must be a str or bytes-like".to_owned())),
    }
}

/// `f` on the bytes of the bytes-like argument `o`, read where they are
/// (unpacking a few bytes of a large buffer copies none of it).
fn with_buffer<R>(o: &Object, f: impl FnOnce(&[u8]) -> R) -> Result<R, RuntimeError> {
    // (CPython asks for a C-contiguous buffer: a strided view is refused,
    // not gathered.)
    if let Object::MemoryView(mv) = o {
        if mv.released.get() {
            return Err(value_error(
                "operation forbidden on released memoryview object",
            ));
        }
        if !mv.is_c_contiguous() {
            return Err(crate::error::buffer_error(
                "memoryview: underlying buffer is not C-contiguous",
            ));
        }
    }
    o.with_bytes_view(f)
        .ok_or_else(|| type_error("argument must be bytes-like".to_owned()))
}

fn b_calcsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = compiled(&fmt)?;
    Ok(Object::Int(cf.size as i64))
}

/// Return one format character per *value slot* the format consumes, in
/// order (`x` pad bytes contribute nothing; `s`/`p` contribute a single
/// slot; numeric codes contribute `count` slots). The frozen wrapper uses
/// this to coerce each argument through the right protocol (`__index__`
/// for integer codes, `__float__` for floats, `__bool__` for `?`) before
/// handing concrete `int`/`float`/`bool` values to the Rust packer, which
/// has no interpreter access of its own.
fn b_value_codes(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = compiled(&fmt)?;
    let mut s = String::new();
    for f in &cf.fields {
        match f.code {
            'x' => {}
            's' | 'p' => s.push(f.code),
            c => {
                for _ in 0..f.count {
                    s.push(c);
                }
            }
        }
    }
    Ok(Object::from_str(s))
}

fn b_pack(args: &[Object]) -> Result<Object, RuntimeError> {
    let cf = format_of(args)?;
    let bytes = cf.pack(&args[1..])?;
    Ok(Object::new_bytes(bytes))
}

/// The compiled format `args[0]` names: a `str` by its identity (a call
/// site's literal is one string every call), a bytes-like one by text.
fn format_of(args: &[Object]) -> Result<std::rc::Rc<CompiledFormat>, RuntimeError> {
    if let Some(Object::Str(s)) = args.first() {
        if let Some(cf) = compiled_for(s) {
            return Ok(cf);
        }
    }
    compiled(&fmt_arg(args, 0)?)
}

/// `struct.pack(format, *v)`'s fast half: a `str` format and plain values
/// packed at once; `None` (nothing done) for anything else or any error,
/// which the Python function raises as `struct.error`.
fn pack_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (Some(Object::Str(fmt)), Some(values)) = (args.first(), args.get(1..)) else {
        return None;
    };
    let plain = values.iter().all(|v| {
        matches!(
            v,
            Object::Int(_)
                | Object::Long(_)
                | Object::Bool(_)
                | Object::Float(_)
                | Object::Complex(_)
                | Object::Str(_)
                | Object::Bytes(_)
                | Object::ByteArray(_)
        )
    });
    if !plain {
        return None;
    }
    let bytes = compiled_for(fmt)?.pack(values).ok()?;
    Some(Ok(Object::new_bytes(bytes)))
}

/// `_native_pack(python_pack)`: the public `struct.pack`, which packs
/// plain values in place (see [`pack_fast`]) and calls the Python
/// function `python_pack` for anything else.
fn b_native_pack(args: &[Object]) -> Result<Object, RuntimeError> {
    let [fallback] = args else {
        return Err(type_error("_native_pack() takes the Python pack"));
    };
    let fallback = fallback.clone();
    let b = Rc::new(BuiltinFn {
        name: "pack",
        binds_instance: false,
        call: Box::new(move |a: &[Object]| match pack_fast(a) {
            Some(r) => r,
            None => {
                let interp = crate::builtins::reentrant_interp()?;
                let globals = interp.builtins_dict();
                interp.call(&fallback, a, &[], &globals)
            }
        }),
        call_kw: None,
    });
    crate::leaf_builtins::register_fast(&b, pack_fast);
    Ok(Object::Builtin(b))
}

/// `struct.unpack(format, buffer)`'s fast half: a `str` format over a
/// `bytes` or `bytearray` buffer, read at once; `None` (nothing done) for anything else
/// or any error, which the Python function raises as `struct.error`.
fn unpack_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Str(fmt), buffer] = args else {
        return None;
    };
    let vals = match buffer {
        Object::Bytes(b) => compiled_for(fmt)?.unpack(b).ok()?,
        Object::ByteArray(b) => compiled_for(fmt)?.unpack(&b.try_borrow().ok()?).ok()?,
        _ => return None,
    };
    Some(Ok(Object::new_tuple(vals)))
}

/// `_native_unpack(python_unpack)`: the public `struct.unpack`, which
/// reads a `bytes` buffer in place (see [`unpack_fast`]) and calls the
/// Python function `python_unpack` for anything else.
fn b_native_unpack(args: &[Object]) -> Result<Object, RuntimeError> {
    let [fallback] = args else {
        return Err(type_error("_native_unpack() takes the Python unpack"));
    };
    let fallback = fallback.clone();
    let b = Rc::new(BuiltinFn {
        name: "unpack",
        binds_instance: false,
        call: Box::new(move |a: &[Object]| match unpack_fast(a) {
            Some(r) => r,
            None => {
                let interp = crate::builtins::reentrant_interp()?;
                let globals = interp.builtins_dict();
                interp.call(&fallback, a, &[], &globals)
            }
        }),
        call_kw: None,
    });
    crate::leaf_builtins::register_fast(&b, unpack_fast);
    let obj = Object::Builtin(b);
    crate::descr_registry::register_text_signature(&obj, "(format, buffer, /)");
    Ok(obj)
}

fn b_unpack(args: &[Object]) -> Result<Object, RuntimeError> {
    let cf = format_of(args)?;
    // (A `bytes` buffer is read in place.)
    let vals = match &args[1] {
        Object::Bytes(b) => cf.unpack(b)?,
        other => with_buffer(other, |buf| cf.unpack(buf))??,
    };
    Ok(Object::new_tuple(vals))
}

/// Resolve a `pack_into`/`unpack_from` byte offset, matching CPython's
/// `Py_ssize_t` coercion: ints (and `bool`) pass through, an int too big
/// for the platform word is `OverflowError`, and any non-integer is a
/// `TypeError` (test_struct.test_pack_into's bogus-offset cases).
fn ssize_offset(o: &Object) -> Result<i64, RuntimeError> {
    match o {
        Object::Int(n) => Ok(*n),
        Object::Bool(b) => Ok(i64::from(*b)),
        Object::Long(_) => Err(overflow_error(
            "Python int too large to convert to C ssize_t",
        )),
        // (Anything else through `__index__`, as CPython's `n` converter
        // takes it; an object without one is a `TypeError`.)
        other => crate::builtins::coerce_index_i64(other),
    }
}

fn b_pack_into(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 3 {
        return Err(type_error("pack_into() requires at least 3 arguments"));
    }
    let fmt = fmt_arg(args, 0)?;
    let cf = compiled(&fmt)?;
    let offset = ssize_offset(&args[2])?;
    let bytes = cf.pack(&args[3..])?;
    match &args[1] {
        Object::ByteArray(buf) => {
            let mut buf = buf.borrow_mut();
            // Resolve the (possibly negative) offset against the buffer
            // and bounds-check without growing it — CPython's
            // `pack_into` writes in place and never resizes.
            let off = resolve_buffer_offset(offset, buf.len(), cf.size, "pack_into", true)?;
            buf[off..off + bytes.len()].copy_from_slice(&bytes);
            Ok(Object::None)
        }
        Object::MemoryView(mv) => {
            // Writable buffer-protocol target (e.g. `memoryview(array(...))`).
            if mv.readonly.get() {
                return Err(type_error("cannot modify read-only memory".to_owned()));
            }
            let off = resolve_buffer_offset(offset, mv.len.get(), cf.size, "pack_into", true)?;
            let base = mv.start.get();
            mv.buffer
                .with_write(|b| b[base + off..base + off + bytes.len()].copy_from_slice(&bytes))
                .map(|()| Object::None)
                .ok_or_else(|| type_error("cannot modify read-only memory".to_owned()))
        }
        _ => Err(type_error(
            "argument must be a read-write bytes-like object".to_owned(),
        )),
    }
}

/// Resolve a `pack_into`/`unpack_from` offset against a buffer of
/// `buf_len` bytes, matching CPython's `_struct` boundary diagnostics.
/// `size` is the struct's byte size; `for_pack` toggles the
/// pack- vs unpack-flavoured messages. Returns the non-negative byte
/// offset to start at, or a `struct.error` describing the overflow.
fn resolve_buffer_offset(
    offset: i64,
    buf_len: usize,
    size: usize,
    op: &str,
    for_pack: bool,
) -> Result<usize, RuntimeError> {
    let size_i = size as i128;
    let len_i = buf_len as i128;
    let off = i128::from(offset);
    let resolved = if off < 0 {
        if off + size_i > 0 {
            let verb = if for_pack { "pack" } else { "unpack" };
            let lead = if for_pack {
                "no space to"
            } else {
                "not enough data to"
            };
            return Err(struct_error(format!(
                "{lead} {verb} {size} bytes at offset {offset}"
            )));
        }
        if off + len_i < 0 {
            return Err(struct_error(format!(
                "offset {offset} out of range for {buf_len}-byte buffer"
            )));
        }
        off + len_i
    } else {
        off
    };
    // `resolved` is now non-negative. Check that `size` bytes fit.
    if len_i - resolved < size_i {
        let needed = (resolved as u128) + (size as u128);
        let verb = if for_pack { "packing" } else { "unpacking" };
        return Err(struct_error(format!(
            "{op} requires a buffer of at least {needed} bytes for \
             {verb} {size} bytes at offset {resolved} \
             (actual buffer size is {buf_len})"
        )));
    }
    Ok(resolved as usize)
}

fn b_unpack_from(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("unpack_from() requires at least 2 arguments"));
    }
    let fmt = fmt_arg(args, 0)?;
    let cf = compiled(&fmt)?;
    let offset = match args.get(2) {
        Some(o) => ssize_offset(o)?,
        None => 0,
    };
    let vals = with_buffer(&args[1], |buf| {
        let off = resolve_buffer_offset(offset, buf.len(), cf.size, "unpack_from", false)?;
        cf.unpack_from_offset(buf, off).map(|(vals, _)| vals)
    })??;
    Ok(Object::new_tuple(vals))
}

/// `struct.unpack_from(format, buffer, offset=0)`'s fast half: a `str`
/// format over a `bytes`, `bytearray` or live contiguous `memoryview`
/// with a plain `int` offset, read where the bytes are; `None` (nothing
/// done) for anything else or any error, which the Python function raises
/// as `struct.error`.
fn unpack_from_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (fmt, buffer, offset) = match args {
        [Object::Str(fmt), buffer] => (fmt, buffer, 0),
        [Object::Str(fmt), buffer, Object::Int(offset)] => (fmt, buffer, *offset),
        _ => return None,
    };
    let read = |buf: &[u8]| {
        let cf = compiled_for(fmt)?;
        let off = resolve_buffer_offset(offset, buf.len(), cf.size, "unpack_from", false).ok()?;
        cf.unpack_from_offset(buf, off).ok().map(|(vals, _)| vals)
    };
    let vals = match buffer {
        Object::Bytes(b) => read(b)?,
        Object::ByteArray(b) => read(&b.try_borrow().ok()?)?,
        Object::MemoryView(mv) if !mv.released.get() && mv.is_c_contiguous() => {
            mv.with_bytes(read)?
        }
        _ => return None,
    };
    Some(Ok(Object::new_tuple(vals)))
}

/// `struct.pack_into(format, buffer, offset, *v)`'s fast half: a `str`
/// format and plain values written into a `bytearray` or a live, writable,
/// contiguous `memoryview` at a plain `int` offset; `None` (nothing done)
/// for anything else or any error.
fn pack_into_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Str(fmt), target, Object::Int(offset), values @ ..] = args else {
        return None;
    };
    if !values
        .iter()
        .all(|v| matches!(v, Object::Int(_) | Object::Bool(_) | Object::Float(_)))
    {
        return None;
    }
    let cf = compiled_for(fmt)?;
    let bytes = cf.pack(values).ok()?;
    let write = |buf: &mut [u8]| {
        let off = resolve_buffer_offset(*offset, buf.len(), cf.size, "pack_into", true).ok()?;
        buf[off..off + bytes.len()].copy_from_slice(&bytes);
        Some(())
    };
    match target {
        Object::ByteArray(b) => write(&mut b.try_borrow_mut().ok()?)?,
        Object::MemoryView(mv)
            if !mv.released.get() && !mv.readonly.get() && mv.is_c_contiguous() =>
        {
            let (start, len) = (mv.start.get(), mv.len.get());
            mv.buffer
                .with_write(|all| write(all.get_mut(start..start + len)?))??;
        }
        _ => return None,
    }
    Some(Ok(Object::None))
}

/// A module function `name` that tries `fast` and calls the Python
/// function `fallback` (with the same arguments) for anything `fast`
/// leaves: the public `struct.unpack_from` and `struct.pack_into`.
fn native_with_fallback(
    name: &'static str,
    fast: fn(&[Object]) -> Option<Result<Object, RuntimeError>>,
    fallback: &Object,
) -> Object {
    let (plain, keyed) = (fallback.clone(), fallback.clone());
    let b = Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |a: &[Object]| match fast(a) {
            Some(r) => r,
            None => {
                let interp = crate::builtins::reentrant_interp()?;
                let globals = interp.builtins_dict();
                interp.call(&plain, a, &[], &globals)
            }
        }),
        call_kw: Some(Box::new(move |a: &[Object], kw: &[(String, Object)]| {
            if kw.is_empty() {
                if let Some(r) = fast(a) {
                    return r;
                }
            }
            let interp = crate::builtins::reentrant_interp()?;
            let globals = interp.builtins_dict();
            interp.call(&keyed, a, kw, &globals)
        })),
    });
    crate::leaf_builtins::register_fast(&b, fast);
    Object::Builtin(b)
}

/// `_native_unpack_from(python_unpack_from)`: the public
/// `struct.unpack_from` (see [`unpack_from_fast`]).
fn b_native_unpack_from(args: &[Object]) -> Result<Object, RuntimeError> {
    let [fallback] = args else {
        return Err(type_error(
            "_native_unpack_from() takes the Python unpack_from",
        ));
    };
    let obj = native_with_fallback("unpack_from", unpack_from_fast, fallback);
    crate::descr_registry::register_text_signature(&obj, "(format, /, buffer, offset=0)");
    Ok(obj)
}

/// `_native_pack_into(python_pack_into)`: the public `struct.pack_into`
/// (see [`pack_into_fast`]).
fn b_native_pack_into(args: &[Object]) -> Result<Object, RuntimeError> {
    let [fallback] = args else {
        return Err(type_error("_native_pack_into() takes the Python pack_into"));
    };
    let obj = native_with_fallback("pack_into", pack_into_fast, fallback);
    crate::descr_registry::register_text_signature(&obj, "(format, buffer, offset, /, *v)");
    Ok(obj)
}

fn b_iter_unpack(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = compiled(&fmt)?;
    let groups = with_buffer(&args[1], |buf| cf.iter_unpack(buf))??;
    let items: Vec<Object> = groups.into_iter().map(Object::new_tuple).collect();
    // Frozen wrapper turns this into an iterator; for now we hand
    // back a list so the Python side can `iter(...)` over it.
    Ok(Object::new_list(items))
}
