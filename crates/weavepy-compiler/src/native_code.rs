//! A compact, WeavePy-internal serialization of [`CodeObject`]s.
//!
//! The private frozen-stdlib cache stores compiled modules in this form
//! rather than as CPython `marshal` data: reading it back is a straight
//! copy of each field, with no CPython bytecode to parse and transcode.
//! It isn't an interchange format. Its layout is free to change with any
//! release, so a reader must check the version byte and treat any
//! mismatch or malformed input as a cache miss.
//!
//! Filenames aren't stored: every code object in a cached module shares
//! the module's filename, which the reader supplies.

use std::sync::Arc;

use crate::bytecode::{CacheTable, Instruction, OpCode};
use crate::{CodeObject, ColSpan, Constant, ExcHandler};

/// The layout revision; bump it whenever the encoding changes.
pub const VERSION: u8 = 3;

/// Encode `code` (and its nested code objects). `None` for a code object
/// the format doesn't carry: one with raw CPython wire overrides, or with
/// a constant that has no fixed value.
pub fn encode(code: &CodeObject) -> Option<Vec<u8>> {
    let mut w = Writer(Vec::with_capacity(4096));
    w.byte(VERSION);
    w.code(code)?;
    Some(w.0)
}

/// Decode a column-span block [`encode`] wrote (see `ColTable`).
pub(crate) fn decode_coltable(bytes: &[u8]) -> Option<Vec<ColSpan>> {
    let mut r = Reader { bytes, pos: 0 };
    let end_lines = r.deltas()?;
    let mut spans = Vec::with_capacity(end_lines.len());
    for end_lineno in end_lines {
        spans.push(ColSpan {
            end_lineno,
            col: r.i32()?,
            end_col: r.i32()?,
        });
    }
    (r.pos == bytes.len()).then_some(spans)
}

/// Decode a line-number block [`encode`] wrote (see `LineTable`).
pub(crate) fn decode_linetable(bytes: &[u8]) -> Option<Vec<u32>> {
    let mut r = Reader { bytes, pos: 0 };
    let lines = r.deltas()?;
    (r.pos == bytes.len()).then_some(lines)
}

/// Decode what [`encode`] wrote, stamping `filename` on every code
/// object. `None` for input it didn't write (or a different version's).
pub fn decode(bytes: &[u8], filename: &str) -> Option<CodeObject> {
    let mut r = Reader { bytes, pos: 0 };
    if r.byte()? != VERSION {
        return None;
    }
    let code = r.code(filename, 0)?;
    (r.pos == bytes.len()).then_some(code)
}

struct Writer(Vec<u8>);

impl Writer {
    fn byte(&mut self, b: u8) {
        self.0.push(b);
    }

    fn uint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }

    fn int(&mut self, v: i64) {
        self.uint(((v << 1) ^ (v >> 63)) as u64);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.uint(b.len() as u64);
        self.0.extend_from_slice(b);
    }

    fn strs(&mut self, v: &[String]) {
        self.uint(v.len() as u64);
        for s in v {
            self.bytes(s.as_bytes());
        }
    }

    fn u32s(&mut self, v: &[u32]) {
        self.uint(v.len() as u64);
        for &x in v {
            self.uint(u64::from(x));
        }
    }

    fn deltas(&mut self, v: impl ExactSizeIterator<Item = u32>) {
        self.uint(v.len() as u64);
        let mut prev = 0i64;
        for x in v {
            self.int(i64::from(x) - prev);
            prev = i64::from(x);
        }
    }

    fn code(&mut self, c: &CodeObject) -> Option<()> {
        if c.wire.is_some() {
            return None;
        }
        self.bytes(c.name.as_bytes());
        self.bytes(c.qualname.as_bytes());
        self.uint(c.instructions.len() as u64);
        for ins in &c.instructions {
            self.byte(ins.op as u8);
            self.uint(u64::from(ins.arg));
        }
        self.uint(c.constants.len() as u64);
        for k in &c.constants {
            self.constant(k)?;
        }
        self.strs(&c.names);
        self.strs(&c.varnames);
        self.strs(&c.freevars);
        self.strs(&c.cellvars);
        self.uint(c.exception_table.len() as u64);
        for h in &c.exception_table {
            self.uint(u64::from(h.start));
            self.uint(u64::from(h.end));
            self.uint(u64::from(h.handler));
            self.uint(u64::from(h.depth));
            self.byte(u8::from(h.push_lasti));
        }
        // Line numbers as deltas from the previous entry: mostly zero, so
        // one byte each whatever the line.
        self.deltas(c.linetable.iter().copied());
        // The column spans as one length-prefixed block, which a reader
        // keeps encoded until something reads a column (see `ColTable`).
        let mut cols = Writer(Vec::new());
        cols.deltas(c.coltable.iter().map(|s| s.end_lineno));
        for s in c.coltable.iter() {
            cols.int(i64::from(s.col));
            cols.int(i64::from(s.end_col));
        }
        self.bytes(&cols.0);
        self.uint(u64::from(c.arg_count));
        self.uint(u64::from(c.posonly_count));
        self.uint(u64::from(c.kwonly_count));
        let flags = [
            c.has_varargs,
            c.has_varkeywords,
            c.is_class_body,
            c.is_generator,
            c.is_coroutine,
            c.is_async_generator,
            c.is_iterable_coroutine,
            c.has_docstring,
            c.is_method,
            c.is_nested,
            c.annotate_scope,
        ];
        let bits = flags
            .iter()
            .enumerate()
            .fold(0u64, |acc, (i, &f)| acc | (u64::from(f) << i));
        self.uint(bits);
        self.uint(u64::from(c.future_flags));
        self.uint(c.stacksize.map_or(0, |s| u64::from(s) + 1));
        self.u32s(&c.no_interrupt_jumps);
        self.bytes(&c.wire_marks);
        self.strs(&c.hidden_locals);
        self.strs(&c.const_identifiers);
        Some(())
    }

    fn constant(&mut self, k: &Constant) -> Option<()> {
        match k {
            Constant::None => self.byte(0),
            Constant::Bool(b) => self.byte(1 + u8::from(*b)),
            Constant::Int(i) => {
                self.byte(3);
                self.int(*i);
            }
            Constant::BigInt(b) => {
                self.byte(4);
                self.bytes(&b.to_signed_bytes_le());
            }
            Constant::Float(x) => {
                self.byte(5);
                self.0.extend_from_slice(&x.to_bits().to_le_bytes());
            }
            Constant::Complex(re, im) => {
                self.byte(6);
                self.0.extend_from_slice(&re.to_bits().to_le_bytes());
                self.0.extend_from_slice(&im.to_bits().to_le_bytes());
            }
            Constant::Str(s) => {
                self.byte(7);
                self.bytes(s.as_bytes());
            }
            Constant::WStr(cps) => {
                self.byte(8);
                self.u32s(cps);
            }
            Constant::Bytes(b) => {
                self.byte(9);
                self.bytes(b);
            }
            Constant::Tuple(items) | Constant::FrozenSet(items) => {
                self.byte(if matches!(k, Constant::Tuple(_)) {
                    10
                } else {
                    11
                });
                self.uint(items.len() as u64);
                for it in items {
                    self.constant(it)?;
                }
            }
            Constant::Code(c) => {
                self.byte(12);
                self.code(c)?;
            }
            Constant::Ellipsis => self.byte(13),
            Constant::Slice(parts) => {
                self.byte(14);
                self.constant(&parts.0)?;
                self.constant(&parts.1)?;
                self.constant(&parts.2)?;
            }
            Constant::Unmarshallable => return None,
        }
        Some(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// Nested code objects deeper than this are taken as corrupt input.
const MAX_NESTING: u32 = 200;

impl Reader<'_> {
    #[inline(always)]
    fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    #[inline(always)]
    fn uint(&mut self) -> Option<u64> {
        // Most values (opcodes' arguments, line numbers, lengths) fit
        // in one byte.
        let b = self.byte()?;
        if b < 0x80 {
            return Some(u64::from(b));
        }
        self.uint_long(b)
    }

    #[inline(never)]
    fn uint_long(&mut self, first: u8) -> Option<u64> {
        let mut v = u64::from(first & 0x7f);
        let mut shift = 7;
        loop {
            let b = self.byte()?;
            if shift >= 64 {
                return None;
            }
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Some(v);
            }
            shift += 7;
        }
    }

    #[inline(always)]
    fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.uint()?).ok()
    }

    #[inline(always)]
    fn int(&mut self) -> Option<i64> {
        let v = self.uint()?;
        Some(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    #[inline(always)]
    fn i32(&mut self) -> Option<i32> {
        i32::try_from(self.int()?).ok()
    }

    /// A length prefix, bounded by what's left of the input (every
    /// element takes at least one byte).
    #[inline(always)]
    fn len(&mut self) -> Option<usize> {
        let n = usize::try_from(self.uint()?).ok()?;
        (n <= self.bytes.len() - self.pos).then_some(n)
    }

    #[inline(always)]
    fn raw(&mut self) -> Option<&[u8]> {
        let n = self.len()?;
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    fn string(&mut self) -> Option<String> {
        Some(std::str::from_utf8(self.raw()?).ok()?.to_owned())
    }

    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.len()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.string()?);
        }
        Some(v)
    }

    fn u32s(&mut self) -> Option<Vec<u32>> {
        let n = self.len()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.u32()?);
        }
        Some(v)
    }

    fn deltas(&mut self) -> Option<Vec<u32>> {
        let n = self.len()?;
        let mut v = Vec::with_capacity(n);
        let mut prev = 0i64;
        for _ in 0..n {
            prev = prev.checked_add(self.int()?)?;
            v.push(u32::try_from(prev).ok()?);
        }
        Some(v)
    }

    /// The bytes of a [`Self::deltas`] block, checked as that would but
    /// left encoded (see `LineTable`).
    fn deltas_raw(&mut self) -> Option<&[u8]> {
        let start = self.pos;
        let n = self.len()?;
        let mut prev = 0i64;
        for _ in 0..n {
            prev = prev.checked_add(self.int()?)?;
            u32::try_from(prev).ok()?;
        }
        Some(&self.bytes[start..self.pos])
    }

    fn f64(&mut self) -> Option<f64> {
        let b = self.bytes.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(f64::from_bits(u64::from_le_bytes(b.try_into().ok()?)))
    }

    fn code(&mut self, filename: &str, depth: u32) -> Option<CodeObject> {
        if depth > MAX_NESTING {
            return None;
        }
        let name = self.string()?;
        let qualname = self.string()?;
        let n = self.len()?;
        let mut instructions = Vec::with_capacity(n);
        for _ in 0..n {
            let op = OpCode::from_u8(self.byte()?)?;
            instructions.push(Instruction::new(op, self.u32()?));
        }
        let n = self.len()?;
        let mut constants = Vec::with_capacity(n);
        for _ in 0..n {
            constants.push(self.constant(filename, depth)?);
        }
        let names = self.strs()?;
        let varnames = self.strs()?;
        let freevars = self.strs()?;
        let cellvars = self.strs()?;
        let n = self.len()?;
        let mut exception_table = Vec::with_capacity(n);
        for _ in 0..n {
            exception_table.push(ExcHandler {
                start: self.u32()?,
                end: self.u32()?,
                handler: self.u32()?,
                depth: self.u32()?,
                push_lasti: self.byte()? != 0,
            });
        }
        let linetable = crate::LineTable::encoded(Arc::from(self.deltas_raw()?));
        let coltable = crate::ColTable::encoded(Arc::from(self.raw()?));
        let arg_count = self.u32()?;
        let posonly_count = self.u32()?;
        let kwonly_count = self.u32()?;
        let bits = self.uint()?;
        let flag = |i: u32| bits & (1 << i) != 0;
        let future_flags = self.u32()?;
        let stacksize = self.u32()?.checked_sub(1);
        let no_interrupt_jumps = self.u32s()?;
        let wire_marks = self.raw()?.to_vec();
        let hidden_locals = self.strs()?;
        let const_identifiers = self.strs()?;
        Some(CodeObject {
            name,
            qualname,
            filename: filename.to_owned(),
            caches: CacheTable::with_len(instructions.len()),
            instructions,
            constants,
            names,
            varnames,
            freevars,
            cellvars,
            exception_table,
            linetable,
            coltable,
            arg_count,
            posonly_count,
            kwonly_count,
            has_varargs: flag(0),
            has_varkeywords: flag(1),
            is_class_body: flag(2),
            is_generator: flag(3),
            is_coroutine: flag(4),
            is_async_generator: flag(5),
            is_iterable_coroutine: flag(6),
            has_docstring: flag(7),
            is_method: flag(8),
            is_nested: flag(9),
            annotate_scope: flag(10),
            future_flags,
            stacksize,
            no_interrupt_jumps,
            wire_marks,
            hidden_locals,
            const_identifiers,
            ..CodeObject::default()
        })
    }

    fn constant(&mut self, filename: &str, depth: u32) -> Option<Constant> {
        Some(match self.byte()? {
            0 => Constant::None,
            1 => Constant::Bool(false),
            2 => Constant::Bool(true),
            3 => Constant::Int(self.int()?),
            4 => Constant::BigInt(num_bigint::BigInt::from_signed_bytes_le(self.raw()?)),
            5 => Constant::Float(self.f64()?),
            6 => Constant::Complex(self.f64()?, self.f64()?),
            7 => Constant::Str(self.string()?),
            8 => Constant::WStr(self.u32s()?),
            9 => Constant::Bytes(self.raw()?.to_vec()),
            tag @ (10 | 11) => {
                let n = self.len()?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.constant(filename, depth)?);
                }
                if tag == 10 {
                    Constant::Tuple(items)
                } else {
                    Constant::FrozenSet(items)
                }
            }
            12 => Constant::Code(Arc::new(self.code(filename, depth + 1)?)),
            13 => Constant::Ellipsis,
            14 => Constant::Slice(Box::new((
                self.constant(filename, depth)?,
                self.constant(filename, depth)?,
                self.constant(filename, depth)?,
            ))),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_constants_and_metadata() {
        let inner = CodeObject {
            name: "f".to_owned(),
            qualname: "C.f".to_owned(),
            filename: "m.py".to_owned(),
            instructions: vec![
                Instruction::new(OpCode::Resume, 0),
                Instruction::new(OpCode::LoadConst, 300),
                Instruction::new(OpCode::ReturnValue, 0),
            ],
            constants: vec![Constant::Int(-5)],
            varnames: vec!["x".to_owned()],
            linetable: vec![1, 2, 2].into(),
            coltable: vec![ColSpan::default(); 3].into(),
            arg_count: 1,
            is_generator: true,
            is_nested: true,
            stacksize: Some(3),
            ..CodeObject::default()
        };
        let code = CodeObject {
            name: "<module>".to_owned(),
            qualname: "<module>".to_owned(),
            filename: "m.py".to_owned(),
            instructions: vec![Instruction::new(OpCode::Nop, 0)],
            constants: vec![
                Constant::None,
                Constant::Bool(true),
                Constant::Int(i64::MIN),
                Constant::BigInt(num_bigint::BigInt::from(-1) << 100),
                Constant::Float(-0.0),
                Constant::Complex(1.5, f64::INFINITY),
                Constant::Str("héllo".to_owned()),
                Constant::WStr(vec![0x61, 0xd800]),
                Constant::Bytes(vec![0, 255]),
                Constant::Tuple(vec![Constant::Ellipsis, Constant::Str(String::new())]),
                Constant::FrozenSet(vec![Constant::Int(1)]),
                Constant::Slice(Box::new((
                    Constant::Int(1),
                    Constant::None,
                    Constant::Int(-1),
                ))),
                Constant::Code(Arc::new(inner)),
            ],
            names: vec!["print".to_owned()],
            exception_table: vec![ExcHandler {
                start: 0,
                end: 1,
                handler: 1,
                depth: 2,
                push_lasti: true,
            }],
            no_interrupt_jumps: vec![7],
            wire_marks: vec![0, 3],
            hidden_locals: vec!["h".to_owned()],
            const_identifiers: vec!["k".to_owned()],
            future_flags: 0x100,
            ..CodeObject::default()
        };
        let bytes = encode(&code).expect("encodable");
        let back = decode(&bytes, "m.py").expect("decodable");
        assert_eq!(back, code);
    }

    #[test]
    fn rejects_truncated_or_foreign_input() {
        let code = CodeObject {
            name: "<module>".to_owned(),
            instructions: vec![Instruction::new(OpCode::Nop, 0)],
            constants: vec![Constant::Str("x".repeat(40))],
            ..CodeObject::default()
        };
        let bytes = encode(&code).expect("encodable");
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut], "").is_none());
        }
        let mut other = bytes.clone();
        other[0] = VERSION + 1;
        assert!(decode(&other, "").is_none());
        assert!(encode(&CodeObject {
            constants: vec![Constant::Unmarshallable],
            ..CodeObject::default()
        })
        .is_none());
    }
}
