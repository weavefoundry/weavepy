//! `marshal` — internal byte serialisation for Python objects (RFC 0019).
//!
//! Implements the version-4 marshal format used by CPython 3.4+ for
//! `.pyc` files. The on-disk format is *not* compatible with
//! CPython's because the embedded code objects use WeavePy's own
//! bytecode, but the surface and the value-encoding map line up so
//! `marshal.dumps(...)` followed by `marshal.loads(...)` round-trips
//! Python values cleanly.
//!
//! Surface:
//! * `dump(value, file)` / `dumps(value)` — serialise.
//! * `load(file)` / `loads(bytes)` — deserialise.
//! * `version` — the protocol version; always 4 for now.

use crate::sync::Rc;
use crate::sync::RefCell;

use num_bigint::{BigInt, Sign};

use weavepy_compiler::{cpython_code, CacheTable, CodeObject, Constant};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{
    BuiltinFn, DictData, DictKey, FileBackend, Object, PyComplex, PyFile, PyModule,
};

// CPython `co_flags` bits we round-trip (Include/cpython/code.h). Only the
// bits whose meaning WeavePy tracks on its own `CodeObject` are consumed on
// read; the rest are informational (e.g. `dis`/`inspect` flag display).
const CO_OPTIMIZED: u32 = 0x0001;
const CO_NEWLOCALS: u32 = 0x0002;
const CO_VARARGS: u32 = 0x0004;
const CO_VARKEYWORDS: u32 = 0x0008;
const CO_GENERATOR: u32 = 0x0020;
const CO_COROUTINE: u32 = 0x0080;
const CO_ITERABLE_COROUTINE: u32 = 0x0100;
const CO_ASYNC_GENERATOR: u32 = 0x0200;

#[allow(dead_code)]
const TYPE_NULL: u8 = b'0';
const TYPE_NONE: u8 = b'N';
const TYPE_FALSE: u8 = b'F';
const TYPE_TRUE: u8 = b'T';
#[allow(dead_code)]
const TYPE_STOPITER: u8 = b'S';
const TYPE_ELLIPSIS: u8 = b'.';
const TYPE_INT: u8 = b'i';
#[allow(dead_code)]
const TYPE_INT64: u8 = b'I'; // legacy
const TYPE_FLOAT: u8 = b'f';
const TYPE_BINARY_FLOAT: u8 = b'g';
#[allow(dead_code)]
const TYPE_COMPLEX: u8 = b'x';
const TYPE_BINARY_COMPLEX: u8 = b'y';
const TYPE_LONG: u8 = b'l';
const TYPE_STRING: u8 = b's';
const TYPE_INTERNED: u8 = b't';
#[allow(dead_code)]
const TYPE_REF: u8 = b'r';
const TYPE_TUPLE: u8 = b'(';
const TYPE_LIST: u8 = b'[';
const TYPE_DICT: u8 = b'{';
#[allow(dead_code)]
const TYPE_CODE: u8 = b'c';
const TYPE_UNICODE: u8 = b'u';
#[allow(dead_code)]
const TYPE_UNKNOWN: u8 = b'?';
const TYPE_SET: u8 = b'<';
const TYPE_FROZENSET: u8 = b'>';
const TYPE_ASCII: u8 = b'a';
const TYPE_ASCII_INTERNED: u8 = b'A';
const TYPE_SMALL_TUPLE: u8 = b')';
const TYPE_SHORT_ASCII: u8 = b'z';
const TYPE_SHORT_ASCII_INTERNED: u8 = b'Z';

const FLAG_REF: u8 = 0x80;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("marshal"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Read and write WeavePy values in binary format."),
        );
        d.insert(DictKey(Object::from_static("version")), Object::Int(4));
        register(&mut d, "dumps", b_dumps);
        register(&mut d, "loads", b_loads);
        register(&mut d, "dump", b_dump);
        register(&mut d, "load", b_load);
    }
    Rc::new(PyModule {
        name: "marshal".to_owned(),
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

// ---------- public API ----------

pub fn b_dumps(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = args
        .first()
        .ok_or_else(|| type_error("dumps requires a value"))?;
    let mut writer = MarshalWriter::default();
    writer.write_value(value)?;
    Ok(Object::new_bytes(writer.into_bytes()))
}

pub fn b_loads(args: &[Object]) -> Result<Object, RuntimeError> {
    let bytes = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("loads requires bytes-like"))?;
    let mut reader = MarshalReader::new(&bytes);
    reader.read_value()
}

fn b_dump(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("dump() requires (value, file)"));
    }
    let bytes = b_dumps(&args[..1])?;
    let data = match &bytes {
        Object::Bytes(b) => b.to_vec(),
        _ => unreachable!(),
    };
    match &args[1] {
        Object::File(f) => {
            f.write_bytes(&data)?;
            Ok(Object::None)
        }
        _ => Err(type_error("dump() expected a file-like object")),
    }
}

fn b_load(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = args
        .first()
        .ok_or_else(|| type_error("load() requires a file"))?;
    let bytes = match f {
        Object::File(file) => file.read_bytes(None)?,
        _ => return Err(type_error("load() expected a file-like object")),
    };
    let mut reader = MarshalReader::new(&bytes);
    reader.read_value()
}

// ---------- writer ----------

#[derive(Default)]
struct MarshalWriter {
    buf: Vec<u8>,
}

impl MarshalWriter {
    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn write_byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn write_int(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[allow(dead_code)]
    fn write_long(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_value(&mut self, value: &Object) -> Result<(), RuntimeError> {
        match value {
            Object::None => self.write_byte(TYPE_NONE),
            Object::Bool(b) => self.write_byte(if *b { TYPE_TRUE } else { TYPE_FALSE }),
            Object::Int(i) => {
                if let Ok(small) = i32::try_from(*i) {
                    self.write_byte(TYPE_INT);
                    self.write_int(small);
                } else {
                    self.write_long_object(&BigInt::from(*i))?;
                }
            }
            Object::Long(b) => self.write_long_object(b)?,
            Object::Float(f) => {
                self.write_byte(TYPE_BINARY_FLOAT);
                self.buf.extend_from_slice(&f.to_le_bytes());
            }
            Object::Complex(c) => {
                self.write_byte(TYPE_BINARY_COMPLEX);
                self.buf.extend_from_slice(&c.real.to_le_bytes());
                self.buf.extend_from_slice(&c.imag.to_le_bytes());
            }
            Object::Str(s) => {
                let bytes = s.as_bytes();
                if bytes.is_ascii() && bytes.len() <= 255 {
                    self.write_byte(TYPE_SHORT_ASCII);
                    self.buf.push(bytes.len() as u8);
                    self.buf.extend_from_slice(bytes);
                } else {
                    self.write_byte(TYPE_UNICODE);
                    self.write_int(bytes.len() as i32);
                    self.buf.extend_from_slice(bytes);
                }
            }
            Object::Bytes(data) => {
                self.write_byte(TYPE_STRING);
                self.write_int(data.len() as i32);
                self.buf.extend_from_slice(data);
            }
            Object::ByteArray(data) => {
                let bytes = data.borrow();
                self.write_byte(TYPE_STRING);
                self.write_int(bytes.len() as i32);
                self.buf.extend_from_slice(&bytes);
            }
            Object::Tuple(items) => {
                if items.len() < 256 {
                    self.write_byte(TYPE_SMALL_TUPLE);
                    self.buf.push(items.len() as u8);
                } else {
                    self.write_byte(TYPE_TUPLE);
                    self.write_int(items.len() as i32);
                }
                for item in items.iter() {
                    self.write_value(item)?;
                }
            }
            Object::List(items) => {
                let items = items.borrow();
                self.write_byte(TYPE_LIST);
                self.write_int(items.len() as i32);
                for item in items.iter() {
                    self.write_value(item)?;
                }
            }
            Object::Dict(d) => {
                self.write_byte(TYPE_DICT);
                let d = d.borrow();
                for (k, v) in d.iter() {
                    self.write_value(&k.0)?;
                    self.write_value(v)?;
                }
                self.write_byte(TYPE_NULL);
            }
            Object::Set(s) => {
                let s = s.borrow();
                self.write_byte(TYPE_SET);
                self.write_int(s.len() as i32);
                for k in s.iter() {
                    self.write_value(&k.0)?;
                }
            }
            Object::FrozenSet(s) => {
                self.write_byte(TYPE_FROZENSET);
                self.write_int(s.len() as i32);
                for k in s.iter() {
                    self.write_value(&k.0)?;
                }
            }
            Object::Code(co) => {
                self.write_code(co)?;
            }
            // `Ellipsis` (the value of `...`) is a singleton instance of the
            // registry `ellipsis` type. CPython marshals it as `TYPE_ELLIPSIS`;
            // it shows up as a code constant in any module using `...` (stub
            // bodies, typing, the many `test` fixtures `PyZipFile.writepy`
            // compiles). The load side already reconstructs the singleton.
            Object::Instance(inst)
                if Rc::ptr_eq(
                    &inst.cls(),
                    &crate::builtin_types::builtin_types().ellipsis_,
                ) =>
            {
                self.write_byte(TYPE_ELLIPSIS);
            }
            other => {
                return Err(value_error(format!(
                    "marshal: unsupported value type '{}'",
                    other.type_name()
                )));
            }
        }
        Ok(())
    }

    fn write_short(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// `TYPE_LONG` — CPython's exact bigint wire form: a signed count of
    /// 15-bit digits (`PyLong_MARSHAL_SHIFT`) followed by each digit as a
    /// little-endian `short`, least-significant first. Byte-compatible
    /// with CPython 3.13's `marshal` (RFC 0033).
    fn write_long_object(&mut self, b: &BigInt) -> Result<(), RuntimeError> {
        self.write_byte(TYPE_LONG);
        let (signed_count, digits15) = bigint_to_15bit(b);
        self.write_int(signed_count);
        for d in digits15 {
            self.write_short(d);
        }
        Ok(())
    }

    /// `TYPE_CODE` — serialise a code object in CPython 3.13's exact field
    /// order (`Python/marshal.c`). The bytecode itself is WeavePy's, but
    /// re-expressed through the CPython codec so the container, the
    /// location/exception tables, and `co_localsplus*` all match what
    /// CPython would write (RFC 0033).
    fn write_code(&mut self, co: &CodeObject) -> Result<(), RuntimeError> {
        let cp = co.to_cpython();
        self.write_byte(TYPE_CODE);
        self.write_int(co.arg_count as i32);
        self.write_int(co.posonly_count as i32);
        self.write_int(co.kwonly_count as i32);
        self.write_int(cp.stacksize as i32);
        self.write_int(code_flags(co) as i32);
        self.write_value(&Object::new_bytes(cp.co_code))?;
        let consts: Vec<Object> = co
            .constants
            .iter()
            .cloned()
            .map(crate::constant_to_object_public)
            .collect();
        self.write_value(&Object::new_tuple(consts))?;
        self.write_value(&strs_to_tuple(&co.names))?;
        self.write_value(&strs_to_tuple(&cp.localsplusnames))?;
        self.write_value(&Object::new_bytes(cp.localspluskinds))?;
        self.write_value(&Object::from_str(co.filename.clone()))?;
        self.write_value(&Object::from_str(co.name.clone()))?;
        // PEP 3155 qualified name, computed at compile time from lexical
        // nesting (`outer.<locals>.inner`, `C.method`). Round-trips so an
        // unmarshalled function/class keeps a faithful `__qualname__`.
        self.write_value(&Object::from_str(co.qualname.clone()))?;
        self.write_int(cp.firstlineno as i32);
        self.write_value(&Object::new_bytes(cp.co_linetable))?;
        self.write_value(&Object::new_bytes(cp.co_exceptiontable))?;
        Ok(())
    }
}

/// CPython `co_flags` for a WeavePy code object. Module/class bodies are
/// not "optimized" (they use name-based locals); functions are.
fn code_flags(co: &CodeObject) -> u32 {
    let mut f = 0u32;
    if co.is_class_body {
        f |= CO_NEWLOCALS;
    } else if co.name != "<module>" {
        f |= CO_OPTIMIZED | CO_NEWLOCALS;
    }
    if co.has_varargs {
        f |= CO_VARARGS;
    }
    if co.has_varkeywords {
        f |= CO_VARKEYWORDS;
    }
    if co.is_generator {
        f |= CO_GENERATOR;
    }
    if co.is_coroutine {
        f |= CO_COROUTINE;
    }
    if co.is_iterable_coroutine {
        f |= CO_ITERABLE_COROUTINE;
    }
    if co.is_async_generator {
        f |= CO_ASYNC_GENERATOR;
    }
    f
}

/// Pack a `BigInt` into CPython's marshal digit form: a signed count of
/// 15-bit little-endian digits (sign carried by the count; `0` for zero).
fn bigint_to_15bit(b: &BigInt) -> (i32, Vec<u16>) {
    let (sign, u32_digits) = b.to_u32_digits();
    let mut out: Vec<u16> = Vec::new();
    let mut acc: u64 = 0;
    let mut nbits: u32 = 0;
    for d in u32_digits {
        acc |= u64::from(d) << nbits;
        nbits += 32;
        while nbits >= 15 {
            out.push((acc & 0x7FFF) as u16);
            acc >>= 15;
            nbits -= 15;
        }
    }
    if acc != 0 {
        out.push((acc & 0x7FFF) as u16);
    }
    while matches!(out.last(), Some(0)) {
        out.pop();
    }
    let count = out.len() as i32;
    let signed = match sign {
        Sign::Minus => -count,
        _ => count,
    };
    (signed, out)
}

/// Build a `marshal` tuple of interned-string objects.
fn strs_to_tuple(items: &[String]) -> Object {
    Object::new_tuple(items.iter().map(|s| Object::from_str(s.clone())).collect())
}

// ---------- reader ----------

struct MarshalReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> MarshalReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_byte(&mut self) -> Result<u8, RuntimeError> {
        if self.pos >= self.bytes.len() {
            // CPython `r_object`: EOF at an object boundary is
            // EOFError, not ValueError (test_exceptions.testRaising).
            return Err(RuntimeError::PyException(
                crate::error::PyException::from_builtin(
                    "EOFError",
                    "EOF read where object expected",
                ),
            ));
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_int(&mut self) -> Result<i32, RuntimeError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(value_error("bad marshal data: short int"));
        }
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.bytes[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(i32::from_le_bytes(buf))
    }

    fn read_long(&mut self) -> Result<i64, RuntimeError> {
        if self.pos + 8 > self.bytes.len() {
            return Err(value_error("bad marshal data: short long"));
        }
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.bytes[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(i64::from_le_bytes(buf))
    }

    fn read_short(&mut self) -> Result<u16, RuntimeError> {
        if self.pos + 2 > self.bytes.len() {
            return Err(value_error("bad marshal data: short u16"));
        }
        let v = u16::from_le_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_n_bytes(&mut self, n: usize) -> Result<Vec<u8>, RuntimeError> {
        if self.pos + n > self.bytes.len() {
            return Err(value_error("bad marshal data: truncated"));
        }
        let bytes = self.bytes[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(bytes)
    }

    fn read_value(&mut self) -> Result<Object, RuntimeError> {
        let tag = self.read_byte()?;
        let tag = tag & !FLAG_REF;
        match tag {
            TYPE_NULL => Err(value_error("bad marshal data: NULL")),
            TYPE_NONE => Ok(Object::None),
            TYPE_TRUE => Ok(Object::Bool(true)),
            TYPE_FALSE => Ok(Object::Bool(false)),
            TYPE_ELLIPSIS => Ok(crate::vm_singletons::ellipsis()),
            TYPE_INT => {
                let v = self.read_int()?;
                Ok(Object::Int(i64::from(v)))
            }
            TYPE_INT64 => {
                let v = self.read_long()?;
                Ok(Object::Int(v))
            }
            TYPE_FLOAT => {
                let len = self.read_byte()? as usize;
                let bytes = self.read_n_bytes(len)?;
                let s =
                    std::str::from_utf8(&bytes).map_err(|_| value_error("bad marshal float"))?;
                Ok(Object::Float(s.parse().unwrap_or(0.0)))
            }
            TYPE_BINARY_FLOAT => {
                let bytes = self.read_n_bytes(8)?;
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&bytes);
                Ok(Object::Float(f64::from_le_bytes(buf)))
            }
            TYPE_BINARY_COMPLEX => {
                let real = self.read_n_bytes(8)?;
                let imag = self.read_n_bytes(8)?;
                let mut rb = [0u8; 8];
                rb.copy_from_slice(&real);
                let mut ib = [0u8; 8];
                ib.copy_from_slice(&imag);
                Ok(Object::Complex(Rc::new(PyComplex::new(
                    f64::from_le_bytes(rb),
                    f64::from_le_bytes(ib),
                ))))
            }
            TYPE_LONG => {
                // Signed count of 15-bit little-endian digits (CPython
                // marshal). Reassemble as a `BigInt`, then auto-demote.
                let signed_count = self.read_int()?;
                let count = signed_count.unsigned_abs() as usize;
                let mut value = BigInt::from(0);
                for i in 0..count {
                    let digit = self.read_short()?;
                    value += BigInt::from(digit) << (15 * i);
                }
                if signed_count < 0 {
                    value = -value;
                }
                Ok(Object::int_from_bigint(value))
            }
            TYPE_CODE => self.read_code(),
            TYPE_STRING => {
                let len = self.read_int()? as usize;
                let bytes = self.read_n_bytes(len)?;
                Ok(Object::new_bytes(bytes))
            }
            TYPE_UNICODE | TYPE_INTERNED | TYPE_ASCII | TYPE_ASCII_INTERNED => {
                let len = self.read_int()? as usize;
                let bytes = self.read_n_bytes(len)?;
                let s = String::from_utf8(bytes).map_err(|_| value_error("bad marshal string"))?;
                Ok(Object::from_str(s))
            }
            TYPE_SHORT_ASCII | TYPE_SHORT_ASCII_INTERNED => {
                let len = self.read_byte()? as usize;
                let bytes = self.read_n_bytes(len)?;
                let s =
                    String::from_utf8(bytes).map_err(|_| value_error("bad marshal short ascii"))?;
                Ok(Object::from_str(s))
            }
            TYPE_TUPLE => {
                let len = self.read_int()? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                Ok(Object::new_tuple(items))
            }
            TYPE_SMALL_TUPLE => {
                let len = self.read_byte()? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                Ok(Object::new_tuple(items))
            }
            TYPE_LIST => {
                let len = self.read_int()? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                Ok(Object::new_list(items))
            }
            TYPE_DICT => {
                let dict = Object::new_dict();
                if let Object::Dict(d) = &dict {
                    let mut d = d.borrow_mut();
                    loop {
                        // Check next byte for sentinel without consuming.
                        if self.pos >= self.bytes.len() {
                            break;
                        }
                        if (self.bytes[self.pos] & !FLAG_REF) == TYPE_NULL {
                            self.pos += 1;
                            break;
                        }
                        let k = self.read_value()?;
                        let v = self.read_value()?;
                        d.insert(DictKey(k), v);
                    }
                }
                Ok(dict)
            }
            TYPE_SET => {
                let len = self.read_int()? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                Ok(Object::new_set_from(items))
            }
            TYPE_FROZENSET => {
                let len = self.read_int()? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                Ok(Object::new_frozenset_from(items))
            }
            other => Err(value_error(format!("marshal: unknown type tag {other:?}"))),
        }
    }

    /// Read a `TYPE_CODE` body (the tag has already been consumed) and
    /// rebuild an executable WeavePy [`CodeObject`] by inverting the
    /// CPython codec (RFC 0033).
    fn read_code(&mut self) -> Result<Object, RuntimeError> {
        let arg_count = self.read_int()? as u32;
        let posonly_count = self.read_int()? as u32;
        let kwonly_count = self.read_int()? as u32;
        let _stacksize = self.read_int()?;
        let flags = self.read_int()? as u32;
        let co_code = self.read_value()?;
        let consts = self.read_value()?;
        let names = self.read_value()?;
        let localsplusnames = self.read_value()?;
        let localspluskinds = self.read_value()?;
        let filename = self.read_value()?;
        let name = self.read_value()?;
        let qualname = self.read_value()?;
        let firstlineno = self.read_int()? as u32;
        let linetable = self.read_value()?;
        let exceptiontable = self.read_value()?;

        let code_bytes = bytes_of(&co_code, "co_code")?;
        let line_bytes = bytes_of(&linetable, "co_linetable")?;
        let exc_bytes = bytes_of(&exceptiontable, "co_exceptiontable")?;
        let lpn = tuple_of_strings(&localsplusnames, "co_localsplusnames")?;
        let lpk = bytes_of(&localspluskinds, "co_localspluskinds")?;

        let decoded = cpython_code::decode_full(
            &code_bytes,
            &line_bytes,
            &exc_bytes,
            &lpn,
            &lpk,
            firstlineno,
        )
        .ok_or_else(|| value_error("marshal: code object uses an unsupported opcode"))?;

        let co_name = string_of(&name, "co_name")?;
        // Fall back to the bare name when the producer didn't record a
        // qualname (e.g. older marshal payloads); CPython always writes one.
        let co_qualname = string_of(&qualname, "co_qualname").unwrap_or_else(|_| co_name.clone());
        let co = CodeObject {
            name: co_name,
            qualname: co_qualname,
            filename: string_of(&filename, "co_filename")?,
            caches: CacheTable::with_len(decoded.instructions.len()),
            instructions: decoded.instructions,
            constants: tuple_to_constants(&consts)?,
            names: tuple_of_strings(&names, "co_names")?,
            varnames: decoded.varnames,
            freevars: decoded.freevars,
            cellvars: decoded.cellvars,
            exception_table: decoded.exception_table,
            linetable: decoded.linetable,
            // Marshal doesn't round-trip PEP-657 columns yet; co_positions()
            // on an unmarshalled code object reports lines only.
            coltable: Vec::new(),
            arg_count,
            posonly_count,
            kwonly_count,
            has_varargs: flags & CO_VARARGS != 0,
            has_varkeywords: flags & CO_VARKEYWORDS != 0,
            is_class_body: false,
            is_generator: flags & CO_GENERATOR != 0,
            is_coroutine: flags & CO_COROUTINE != 0,
            is_async_generator: flags & CO_ASYNC_GENERATOR != 0,
            is_iterable_coroutine: flags & CO_ITERABLE_COROUTINE != 0,
        };
        Ok(Object::Code(Rc::new(co)))
    }
}

/// Extract a byte buffer from a marshalled value, or a descriptive error.
fn bytes_of(o: &Object, field: &str) -> Result<Vec<u8>, RuntimeError> {
    o.as_bytes_view()
        .ok_or_else(|| value_error(format!("marshal: code object field '{field}' is not bytes")))
}

/// Extract a `str` from a marshalled value.
fn string_of(o: &Object, field: &str) -> Result<String, RuntimeError> {
    match o {
        Object::Str(s) => Ok(s.to_string()),
        _ => Err(value_error(format!(
            "marshal: code object field '{field}' is not a str"
        ))),
    }
}

/// Extract a tuple of `str` from a marshalled value.
fn tuple_of_strings(o: &Object, field: &str) -> Result<Vec<String>, RuntimeError> {
    match o {
        Object::Tuple(items) => items.iter().map(|x| string_of(x, field)).collect(),
        _ => Err(value_error(format!(
            "marshal: code object field '{field}' is not a tuple"
        ))),
    }
}

/// Fold a marshalled `co_consts` tuple back into compile-time constants.
fn tuple_to_constants(o: &Object) -> Result<Vec<Constant>, RuntimeError> {
    match o {
        Object::Tuple(items) => Ok(items.iter().map(crate::object_to_constant_public).collect()),
        _ => Err(value_error("marshal: code object co_consts is not a tuple")),
    }
}

/// Helper used by the import machinery (RFC 0019 `__pycache__`).
pub fn dump_to_pyfile(value: &Object, file: &PyFile) -> Result<(), RuntimeError> {
    let mut w = MarshalWriter::default();
    w.write_value(value)?;
    file.write_bytes(&w.into_bytes())?;
    Ok(())
}

pub fn load_from_bytes(bytes: &[u8]) -> Result<Object, RuntimeError> {
    let mut r = MarshalReader::new(bytes);
    r.read_value()
}

#[allow(dead_code)]
fn discard_file(_: FileBackend) {}
