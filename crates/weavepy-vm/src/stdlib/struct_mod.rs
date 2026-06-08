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
                    let data = match v {
                        Object::Bytes(b) => b.to_vec(),
                        Object::ByteArray(b) => b.borrow().clone(),
                        Object::Str(s) => s.as_bytes().to_vec(),
                        _ => {
                            return Err(struct_error(
                                "argument for 's' must be a bytes-like or str".to_owned(),
                            ))
                        }
                    };
                    let mut buf = vec![0u8; f.count];
                    let take = data.len().min(f.count);
                    buf[..take].copy_from_slice(&data[..take]);
                    out.extend_from_slice(&buf);
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
        let mut out = Vec::new();
        let mut pos = start;
        for f in &self.fields {
            match f.code {
                'x' => {
                    pos += f.count;
                }
                's' => {
                    let bytes = &buf[pos..pos + f.count];
                    out.push(Object::new_bytes(bytes.to_vec()));
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
                    out.push(Object::new_bytes(bytes.to_vec()));
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
        'i' | 'I' | 'l' | 'L' | 'f' => 4,
        'q' | 'Q' | 'd' => 8,
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
        'i' | 'I' | 'l' | 'L' | 'f' => 4,
        'q' | 'Q' | 'd' => 8,
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
            let bytes = match value {
                Object::Bytes(b) if b.len() == 1 => b.to_vec(),
                Object::Str(s) if s.len() == 1 => s.as_bytes().to_vec(),
                _ => {
                    return Err(struct_error(
                        "argument for 'c' must be a 1-byte bytes/str".to_owned(),
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
        'i' | 'l' => write_int!(i32, write_i32, true),
        'I' | 'L' => write_int!(u32, write_u32, false),
        'q' => write_int!(i64, write_i64, true),
        'Q' => write_int!(u64, write_u64, false),
        'f' => {
            let f = value
                .as_f64()
                .ok_or_else(|| struct_error("required argument is not a float"))?;
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
            let f = value
                .as_f64()
                .ok_or_else(|| struct_error("required argument is not a float"))?;
            let mut buf = [0u8; 8];
            match endian {
                Endian::Native => NativeEndian::write_f64(&mut buf, f),
                Endian::Standard | Endian::Little => LittleEndian::write_f64(&mut buf, f),
                Endian::Big => BigEndian::write_f64(&mut buf, f),
            }
            out.extend_from_slice(&buf);
            Ok(())
        }
        'e' => {
            // Half-precision IEEE 754, converted from the double with
            // round-half-to-even (CPython `_PyFloat_Pack2`), not via an
            // intermediate `f32` truncation.
            let f = value
                .as_f64()
                .ok_or_else(|| struct_error("required argument is not a float"))?;
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
        'i' | 'l' => Object::Int(i64::from(read_i32(endian, &buf[..4]))),
        'I' | 'L' => Object::Int(i64::from(read_u32(endian, &buf[..4]))),
        'q' => Object::Int(read_i64(endian, &buf[..8])),
        'Q' => {
            let v = read_u64(endian, &buf[..8]);
            if i64::try_from(v).is_ok() {
                Object::Int(v as i64)
            } else {
                Object::int_from_bigint(num_bigint::BigInt::from(v))
            }
        }
        'f' => Object::Float(f64::from(read_f32(endian, &buf[..4]))),
        'd' => Object::Float(read_f64(endian, &buf[..8])),
        'e' => Object::Float(f64::from(half_to_f32(read_u16(endian, &buf[..2])))),
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
        _ => Err(struct_error(format!(
            "required argument is not an integer (got '{}')",
            v.type_name()
        ))),
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
    let dict = Rc::new(RefCell::new(DictData::new()));
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
        register(&mut d, "unpack", b_unpack);
        register(&mut d, "pack_into", b_pack_into);
        register(&mut d, "unpack_from", b_unpack_from);
        register(&mut d, "iter_unpack", b_iter_unpack);
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
    let bf = BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    };
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(bf)),
    );
}

fn fmt_arg(args: &[Object], idx: usize) -> Result<String, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(Object::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        _ => Err(type_error("format must be a str or bytes-like".to_owned())),
    }
}

fn buffer_arg(o: &Object) -> Result<Vec<u8>, RuntimeError> {
    o.as_bytes_view()
        .ok_or_else(|| type_error("argument must be bytes-like".to_owned()))
}

fn b_calcsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
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
    let cf = CompiledFormat::parse(&fmt)?;
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
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
    let bytes = cf.pack(&args[1..])?;
    Ok(Object::new_bytes(bytes))
}

fn b_unpack(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
    let buf = buffer_arg(&args[1])?;
    let vals = cf.unpack(&buf)?;
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
        other => Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

fn b_pack_into(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 3 {
        return Err(type_error("pack_into() requires at least 3 arguments"));
    }
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
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
                return Err(type_error(
                    "cannot modify read-only memory".to_owned(),
                ));
            }
            let off = resolve_buffer_offset(offset, mv.len.get(), cf.size, "pack_into", true)?;
            let base = mv.start.get();
            match &mv.buffer {
                crate::object::MemoryViewBuffer::ByteArray(b) => {
                    let mut b = b.borrow_mut();
                    b[base + off..base + off + bytes.len()].copy_from_slice(&bytes);
                    Ok(Object::None)
                }
                crate::object::MemoryViewBuffer::Bytes(_) => Err(type_error(
                    "cannot modify read-only memory".to_owned(),
                )),
            }
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
    let off = offset as i128;
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
    let cf = CompiledFormat::parse(&fmt)?;
    let buf = buffer_arg(&args[1])?;
    let offset = args.get(2).and_then(|o| o.as_i64()).unwrap_or(0);
    let off = resolve_buffer_offset(offset, buf.len(), cf.size, "unpack_from", false)?;
    let (vals, _) = cf.unpack_from_offset(&buf, off)?;
    Ok(Object::new_tuple(vals))
}

fn b_iter_unpack(args: &[Object]) -> Result<Object, RuntimeError> {
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
    let buf = buffer_arg(&args[1])?;
    let groups = cf.iter_unpack(&buf)?;
    let items: Vec<Object> = groups.into_iter().map(Object::new_tuple).collect();
    // Frozen wrapper turns this into an iterator; for now we hand
    // back a list so the Python side can `iter(...)` over it.
    Ok(Object::new_list(items))
}
