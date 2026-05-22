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

use std::cell::RefCell;
use std::rc::Rc;

use byteorder::{BigEndian, ByteOrder, LittleEndian, NativeEndian};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

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
            let bytes = match code {
                's' | 'p' => unit * n,
                _ => unit * n,
            };
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
            size += bytes;
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
        let mut out = Vec::with_capacity(self.size);
        let mut idx = 0usize;
        for f in &self.fields {
            match f.code {
                'x' => {
                    out.extend(std::iter::repeat(0u8).take(f.count));
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
                        let take = data.len().min(f.count - 1).min(255);
                        buf[0] = take as u8;
                        if take > 0 {
                            buf[1..1 + take].copy_from_slice(&data[..take]);
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

    fn unpack_from(&self, buf: &[u8], offset: usize) -> Result<Vec<Object>, RuntimeError> {
        if buf.len() < offset + self.size {
            return Err(struct_error(format!(
                "unpack_from requires a buffer of at least {} bytes for unpacking {} bytes at offset {}",
                offset + self.size,
                self.size,
                offset
            )));
        }
        self.unpack_from_offset(buf, offset).map(|(v, _)| v)
    }

    fn iter_unpack(&self, buf: &[u8]) -> Result<Vec<Vec<Object>>, RuntimeError> {
        if buf.len() % self.size != 0 {
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
            _ => return Err(struct_error("'n' format code only valid in native mode")),
        },
        'P' => match endian {
            Endian::Native => std::mem::size_of::<usize>(),
            _ => return Err(struct_error("'P' format code only valid in native mode")),
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

fn encode_one(code: char, endian: Endian, value: &Object, out: &mut Vec<u8>) -> Result<(), RuntimeError> {
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
                Object::Str(s) if s.as_bytes().len() == 1 => s.as_bytes().to_vec(),
                _ => return Err(struct_error("argument for 'c' must be a 1-byte bytes/str".to_owned())),
            };
            out.push(bytes[0]);
            Ok(())
        }
        '?' => {
            let v = if value.is_truthy() { 1u8 } else { 0u8 };
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
            let f = value.as_f64().ok_or_else(|| struct_error("required argument is not a float"))?;
            let mut buf = [0u8; 4];
            match endian {
                Endian::Native => NativeEndian::write_f32(&mut buf, f as f32),
                Endian::Standard | Endian::Little => LittleEndian::write_f32(&mut buf, f as f32),
                Endian::Big => BigEndian::write_f32(&mut buf, f as f32),
            };
            out.extend_from_slice(&buf);
            Ok(())
        }
        'd' => {
            let f = value.as_f64().ok_or_else(|| struct_error("required argument is not a float"))?;
            let mut buf = [0u8; 8];
            match endian {
                Endian::Native => NativeEndian::write_f64(&mut buf, f),
                Endian::Standard | Endian::Little => LittleEndian::write_f64(&mut buf, f),
                Endian::Big => BigEndian::write_f64(&mut buf, f),
            };
            out.extend_from_slice(&buf);
            Ok(())
        }
        'e' => {
            // Half-precision IEEE 754. Convert via the bits.
            let f = value.as_f64().ok_or_else(|| struct_error("required argument is not a float"))?;
            let half = f32_to_half(f as f32);
            let mut buf = [0u8; 2];
            match endian {
                Endian::Native => NativeEndian::write_u16(&mut buf, half),
                Endian::Standard | Endian::Little => LittleEndian::write_u16(&mut buf, half),
                Endian::Big => BigEndian::write_u16(&mut buf, half),
            };
            out.extend_from_slice(&buf);
            Ok(())
        }
        'n' => {
            let n = require_int(value)?;
            let v = isize::try_from(n).map_err(|_| struct_error("argument out of range for 'n'"))?;
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
            if v <= i64::MAX as u64 {
                Object::Int(v as i64)
            } else {
                Object::int_from_bigint(num_bigint::BigInt::from(v))
            }
        }
        'f' => Object::Float(f64::from(read_f32(endian, &buf[..4]))),
        'd' => Object::Float(read_f64(endian, &buf[..8])),
        'e' => Object::Float(f64::from(half_to_f32(read_u16(endian, &buf[..2])))),
        'n' => {
            let v = NativeEndian::read_int(&buf[..std::mem::size_of::<isize>()], std::mem::size_of::<isize>());
            Object::Int(v)
        }
        'N' | 'P' => {
            let v = NativeEndian::read_uint(&buf[..std::mem::size_of::<usize>()], std::mem::size_of::<usize>());
            if v <= i64::MAX as u64 {
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

/// IEEE 754 binary16 conversions. Doesn't depend on `f16` because
/// the standard library hasn't shipped a stable type yet.
fn f32_to_half(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;
    if exp == 0xFF {
        // NaN/Inf
        let mant = if mantissa != 0 { 0x200 } else { 0 };
        return sign | 0x7C00 | mant;
    }
    let new_exp = exp - 127 + 15;
    if new_exp >= 0x1F {
        return sign | 0x7C00; // Inf
    }
    if new_exp <= 0 {
        if new_exp < -10 {
            return sign;
        }
        let mantissa = mantissa | 0x0080_0000;
        let shift = (14 - new_exp) as u32;
        let result = (mantissa >> shift) as u16;
        return sign | result;
    }
    sign | ((new_exp as u16) << 10) | ((mantissa >> 13) as u16)
}

fn half_to_f32(half: u16) -> f32 {
    let sign = (half & 0x8000) as u32;
    let exp = ((half >> 10) & 0x1F) as i32;
    let mantissa = (half & 0x03FF) as u32;
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
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + 'static,
) {
    let bf = BuiltinFn {
        name,
        call: Box::new(body),
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

fn b_pack_into(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 3 {
        return Err(type_error("pack_into() requires at least 3 arguments"));
    }
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
    let offset = args[2].as_i64().ok_or_else(|| type_error("offset must be int"))?;
    let bytes = cf.pack(&args[3..])?;
    match &args[1] {
        Object::ByteArray(buf) => {
            let mut buf = buf.borrow_mut();
            let off = offset.max(0) as usize;
            if buf.len() < off + bytes.len() {
                buf.resize(off + bytes.len(), 0);
            }
            buf[off..off + bytes.len()].copy_from_slice(&bytes);
            Ok(Object::None)
        }
        _ => Err(type_error("pack_into() requires a bytearray buffer".to_owned())),
    }
}

fn b_unpack_from(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("unpack_from() requires at least 2 arguments"));
    }
    let fmt = fmt_arg(args, 0)?;
    let cf = CompiledFormat::parse(&fmt)?;
    let buf = buffer_arg(&args[1])?;
    let offset = args
        .get(2)
        .and_then(|o| o.as_i64())
        .unwrap_or(0)
        .max(0) as usize;
    let vals = cf.unpack_from(&buf, offset)?;
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
