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
pub const VERSION: u8 = 4;

/// Encode `code` (and its nested code objects). `None` for a code object
/// the format doesn't carry: one with raw CPython wire overrides, or with
/// a constant that has no fixed value.
pub fn encode(code: &CodeObject) -> Option<Vec<u8>> {
    let mut w = Writer(Vec::with_capacity(4096));
    w.byte(VERSION);
    w.code(code)?;
    Some(w.0)
}

/// The most instructions a code object's position tables may describe:
/// a run-length encoded table could otherwise ask for any length.
const MAX_TABLE_LEN: u64 = 1 << 24;

/// Decode a column-span block [`encode`] wrote (see `ColTable`): the
/// span count, then a record per run of equal spans. A record is
/// `run - 1` shifted left past a flag bit; with the flag clear the span
/// is the unknown default, and with it set three numbers follow: the end
/// line's delta from the previous known span's, the column plus one, and
/// the end column's delta from the column.
pub(crate) fn decode_coltable(bytes: &[u8]) -> Option<Vec<ColSpan>> {
    let mut r = Reader { bytes, pos: 0 };
    let n = r.table_len()?;
    let mut spans = Vec::with_capacity(n);
    let mut end_lineno = 0i64;
    while spans.len() < n {
        let tag = r.uint()?;
        let span = if tag & 1 == 0 {
            ColSpan::default()
        } else {
            end_lineno = end_lineno.checked_add(r.int()?)?;
            let col = i64::try_from(r.uint()?).ok()? - 1;
            let end_col = col.checked_add(r.int()?)?;
            ColSpan {
                end_lineno: u32::try_from(end_lineno).ok()?,
                col: i32::try_from(col).ok()?,
                end_col: i32::try_from(end_col).ok()?,
            }
        };
        let run = usize::try_from(tag >> 1).ok()?.checked_add(1)?;
        if run > n - spans.len() {
            return None;
        }
        spans.extend(std::iter::repeat_n(span, run));
    }
    (r.pos == bytes.len()).then_some(spans)
}

/// Decode a line-number block [`encode`] wrote (see `LineTable`).
pub(crate) fn decode_linetable(bytes: &[u8]) -> Option<Vec<u32>> {
    let mut r = Reader { bytes, pos: 0 };
    let mut lines = Vec::new();
    r.line_runs(|line, run| lines.extend(std::iter::repeat_n(line, run)))?;
    (r.pos == bytes.len()).then_some(lines)
}

/// Decode a wire-mark block [`encode`] wrote (see `WireMarks`): the mark
/// count, then the marks two to a byte, the first in the low half.
pub(crate) fn decode_wire_marks(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader { bytes, pos: 0 };
    let n = r.table_len()?;
    let packed = r.bytes.get(r.pos..r.pos.checked_add(n.div_ceil(2))?)?;
    let marks: Vec<u8> = packed
        .iter()
        .flat_map(|&b| [b & 0x0f, b >> 4])
        .take(n)
        .collect();
    (r.pos + packed.len() == bytes.len()).then_some(marks)
}

/// Decode what [`encode`] wrote, stamping `filename` on every code
/// object. `None` for input it didn't write (or a different version's).
///
/// The code objects share one copy of the filename, and one buffer
/// holds all their line and column tables and wire marks (see
/// `LineTable`).
pub fn decode(bytes: &[u8], filename: &str) -> Option<CodeObject> {
    let mut r = Reader { bytes, pos: 0 };
    if r.byte()? != VERSION {
        return None;
    }
    let mut module = Module {
        filename: Arc::from(filename),
        tables: Vec::new(),
        buffer: Arc::new(std::sync::OnceLock::new()),
    };
    let code = r.code(&mut module, 0)?;
    if r.pos != bytes.len() {
        return None;
    }
    // (Copied rather than shrunk in place, which can keep the vector's
    // spare capacity.)
    let _ = module.buffer.set(Box::from(module.tables.as_slice()));
    Some(code)
}

/// What the code objects of one module decoded by [`decode`] share.
struct Module {
    filename: Arc<str>,
    /// Every code object's encoded line and column tables and wire
    /// marks, in order.
    tables: Vec<u8>,
    /// Where `tables` goes once the decoding is done.
    buffer: Arc<std::sync::OnceLock<Box<[u8]>>>,
}

impl Module {
    /// Append an encoded table to the shared buffer.
    fn table(&mut self, bytes: &[u8]) -> Option<crate::Encoded> {
        let start = u32::try_from(self.tables.len()).ok()?;
        self.tables.extend_from_slice(bytes);
        let end = u32::try_from(self.tables.len()).ok()?;
        Some(crate::Encoded::new(self.buffer.clone(), start, end))
    }
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

    /// Line numbers as runs: the count, then for each run of equal lines
    /// its line's delta from the previous run's line and its length.
    fn line_runs(&mut self, lines: &[u32]) {
        self.uint(lines.len() as u64);
        let mut prev = 0i64;
        for run in lines.chunk_by(|a, b| a == b) {
            self.int(i64::from(run[0]) - prev);
            self.uint(run.len() as u64);
            prev = i64::from(run[0]);
        }
    }

    /// Column spans as runs (see [`decode_coltable`]); `None` for a
    /// column below `-1`, which the format has no room for.
    fn col_runs(&mut self, spans: &[ColSpan]) -> Option<()> {
        self.uint(spans.len() as u64);
        let mut end_lineno = 0i64;
        for run in spans.chunk_by(|a, b| a == b) {
            let repeat = (run.len() as u64 - 1) << 1;
            let s = run[0];
            if s == ColSpan::default() {
                self.uint(repeat);
                continue;
            }
            self.uint(repeat | 1);
            self.int(i64::from(s.end_lineno) - end_lineno);
            end_lineno = i64::from(s.end_lineno);
            self.uint(u64::try_from(i64::from(s.col) + 1).ok()?);
            self.int(i64::from(s.end_col) - i64::from(s.col));
        }
        Some(())
    }

    /// Wire marks two to a byte (see [`decode_wire_marks`]); `None` for a
    /// mark that needs more than four bits.
    fn wire_marks(&mut self, marks: &[u8]) -> Option<()> {
        self.uint(marks.len() as u64);
        for pair in marks.chunks(2) {
            let (lo, hi) = (pair[0], pair.get(1).copied().unwrap_or(0));
            if lo > 0x0f || hi > 0x0f {
                return None;
            }
            self.byte(lo | hi << 4);
        }
        Some(())
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
        // Line numbers in runs: a statement's instructions share a line.
        self.line_runs(&c.linetable);
        // The column spans as one length-prefixed block, which a reader
        // keeps encoded until something reads a column (see `ColTable`).
        let mut cols = Writer(Vec::new());
        cols.col_runs(&c.coltable)?;
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
        self.u32s(c.no_interrupt_jumps());
        self.wire_marks(&c.wire_marks)?;
        self.strs(c.hidden_locals());
        self.strs(c.const_identifiers());
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

    /// A position table's length (bounded: see [`MAX_TABLE_LEN`]).
    fn table_len(&mut self) -> Option<usize> {
        let n = self.uint()?;
        (n <= MAX_TABLE_LEN).then(|| n as usize)
    }

    /// Read a line-number block (see [`Writer::line_runs`]), passing each
    /// run's line and length to `run`.
    fn line_runs(&mut self, mut run: impl FnMut(u32, usize)) -> Option<()> {
        let n = self.table_len()?;
        let (mut seen, mut line) = (0usize, 0i64);
        while seen < n {
            line = line.checked_add(self.int()?)?;
            let len = usize::try_from(self.uint()?).ok()?;
            if len == 0 || len > n - seen {
                return None;
            }
            run(u32::try_from(line).ok()?, len);
            seen += len;
        }
        Some(())
    }

    /// The bytes of a line-number block, checked as decoding would but
    /// left encoded (see `LineTable`).
    fn line_runs_raw(&mut self) -> Option<&[u8]> {
        let start = self.pos;
        self.line_runs(|_, _| {})?;
        Some(&self.bytes[start..self.pos])
    }

    /// The bytes of a wire-mark block, checked for length but left
    /// encoded (see `WireMarks`); empty for code without marks.
    fn wire_marks_raw(&mut self) -> Option<&[u8]> {
        let start = self.pos;
        let n = self.table_len()?;
        let end = self.pos.checked_add(n.div_ceil(2))?;
        if end > self.bytes.len() {
            return None;
        }
        self.pos = end;
        Some(if n == 0 { &[] } else { &self.bytes[start..end] })
    }

    fn f64(&mut self) -> Option<f64> {
        let b = self.bytes.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(f64::from_bits(u64::from_le_bytes(b.try_into().ok()?)))
    }

    fn code(&mut self, module: &mut Module, depth: u32) -> Option<CodeObject> {
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
            constants.push(self.constant(module, depth)?);
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
        let linetable = crate::LineTable::encoded(module.table(self.line_runs_raw()?)?);
        let coltable = crate::ColTable::encoded(module.table(self.raw()?)?);
        let arg_count = self.u32()?;
        let posonly_count = self.u32()?;
        let kwonly_count = self.u32()?;
        let bits = self.uint()?;
        let flag = |i: u32| bits & (1 << i) != 0;
        let future_flags = self.u32()?;
        let stacksize = self.u32()?.checked_sub(1);
        let no_interrupt_jumps = self.u32s()?;
        let wire_marks = match self.wire_marks_raw()? {
            [] => crate::WireMarks::default(),
            marks => crate::WireMarks::encoded(module.table(marks)?),
        };
        let mut rare = crate::CodeRare::default();
        rare.set(crate::RareFields {
            no_interrupt_jumps,
            hidden_locals: self.strs()?,
            const_identifiers: self.strs()?,
        });
        Some(CodeObject {
            name,
            qualname,
            filename: module.filename.clone(),
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
            wire_marks,
            rare,
            ..CodeObject::default()
        })
    }

    fn constant(&mut self, module: &mut Module, depth: u32) -> Option<Constant> {
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
                    items.push(self.constant(module, depth)?);
                }
                if tag == 10 {
                    Constant::Tuple(items)
                } else {
                    Constant::FrozenSet(items)
                }
            }
            12 => Constant::Code(Arc::new(self.code(module, depth + 1)?)),
            13 => Constant::Ellipsis,
            14 => Constant::Slice(Box::new((
                self.constant(module, depth)?,
                self.constant(module, depth)?,
                self.constant(module, depth)?,
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
            filename: "m.py".into(),
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
        let mut code = CodeObject {
            name: "<module>".to_owned(),
            qualname: "<module>".to_owned(),
            filename: "m.py".into(),
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
            wire_marks: vec![0, 3].into(),
            future_flags: 0x100,
            ..CodeObject::default()
        };
        code.rare.set(crate::RareFields {
            no_interrupt_jumps: vec![7],
            hidden_locals: vec!["h".to_owned()],
            const_identifiers: vec!["k".to_owned()],
        });
        let bytes = encode(&code).expect("encodable");
        let back = decode(&bytes, "m.py").expect("decodable");
        assert_eq!(back, code);
        // Every code object shares the module's filename.
        let Some(Constant::Code(inner)) = back.constants.last() else {
            panic!("nested code object expected");
        };
        assert!(Arc::ptr_eq(&back.filename, &inner.filename));
        assert_eq!(*inner.linetable, [1, 2, 2]);
    }

    #[test]
    fn round_trips_position_runs() {
        let span = |end_lineno, col, end_col| ColSpan {
            end_lineno,
            col,
            end_col,
        };
        let lines = vec![3, 3, 4, 4, 4, 0, 9, 2, 2];
        let cols = vec![
            span(3, 4, 9),
            span(3, 4, 9),
            ColSpan::default(),
            span(5, 200, 3),
            span(4, 0, 0),
            ColSpan::default(),
            ColSpan::default(),
            span(2, -1, -1),
            span(9, 7, 7),
        ];
        let marks = vec![0, 3, 8, 0, 0, 1, 15, 0, 2];
        let code = CodeObject {
            instructions: vec![Instruction::new(OpCode::Nop, 0); lines.len()],
            linetable: lines.clone().into(),
            coltable: cols.clone().into(),
            wire_marks: marks.clone().into(),
            ..CodeObject::default()
        };
        let back = decode(&encode(&code).expect("encodable"), "m.py").expect("decodable");
        assert_eq!(*back.linetable, lines);
        assert_eq!(*back.coltable, cols);
        assert_eq!(*back.wire_marks, marks);
        // A mark wider than four bits has no encoding.
        let wide = CodeObject {
            wire_marks: vec![16].into(),
            ..CodeObject::default()
        };
        assert!(encode(&wide).is_none());
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
