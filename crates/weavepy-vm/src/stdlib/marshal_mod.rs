//! `marshal` — internal byte serialisation for Python objects (RFC 0019).
//!
//! Implements the version-5 marshal format used by CPython 3.14 for
//! `.pyc` files (version 5 adds `TYPE_SLICE`; versions 0..=4 are still
//! written on request). The on-disk format is *not* compatible with
//! CPython's because the embedded code objects use WeavePy's own
//! bytecode, but the surface and the value-encoding map line up so
//! `marshal.dumps(...)` followed by `marshal.loads(...)` round-trips
//! Python values cleanly.
//!
//! RFC 0060 brought the surface to CPython parity:
//! * `version` argument (versions ≥ 3 share repeated objects through
//!   `FLAG_REF`/`TYPE_REF`, which also makes recursive containers
//!   marshallable);
//! * `allow_code=` keyword on all four entry points;
//! * interned strings round-trip their identity;
//! * buffer-protocol inputs (memoryview, array) dump as bytes;
//! * `load(f)` reads incrementally through `readinto`/`read`, leaving
//!   the file position exactly past the value.
//!
//! Surface:
//! * `dump(value, file[, version])` / `dumps(value[, version])`.
//! * `load(file)` / `loads(bytes)`.
//! * `version` — the protocol version; 5 (RFC 0077 WS9).

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
const CO_VARARGS: u32 = 0x0004;
const CO_VARKEYWORDS: u32 = 0x0008;
const CO_NESTED: u32 = 0x0010;
const CO_GENERATOR: u32 = 0x0020;
const CO_COROUTINE: u32 = 0x0080;
const CO_ITERABLE_COROUTINE: u32 = 0x0100;
const CO_ASYNC_GENERATOR: u32 = 0x0200;
// CPython 3.14: `co_consts[0]` is the docstring / function-like scope
// defined directly in a class body.
const CO_HAS_DOCSTRING: u32 = 0x0400_0000;
const CO_METHOD: u32 = 0x0800_0000;

const TYPE_NULL: u8 = b'0';
const TYPE_NONE: u8 = b'N';
const TYPE_FALSE: u8 = b'F';
const TYPE_TRUE: u8 = b'T';
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
const TYPE_REF: u8 = b'r';
const TYPE_TUPLE: u8 = b'(';
const TYPE_LIST: u8 = b'[';
const TYPE_DICT: u8 = b'{';
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
/// CPython 3.14 (marshal version 5): a `slice` constant, three nested
/// values `start`, `stop`, `step`. Needed because 3.14's compiler folds
/// `a[1:2]` into a slice constant in `co_consts`.
const TYPE_SLICE: u8 = b':';

const FLAG_REF: u8 = 0x80;

/// The version WeavePy writes by default (CPython 3.14's `Py_MARSHAL_VERSION`).
const MARSHAL_VERSION: i64 = 5;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        d.insert(
            DictKey(Object::from_static("version")),
            Object::Int(MARSHAL_VERSION),
        );
        register(&mut d, "dumps", dumps_kw);
        register(&mut d, "loads", loads_kw);
        register(&mut d, "dump", dump_kw);
        register(&mut d, "load", load_kw);
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
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) {
    let bf = BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    };
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(bf)),
    );
}

// ---------- public API ----------

/// The `allow_code=` keyword shared by all four entry points; any other
/// keyword raises `TypeError` like CPython's argument clinic.
fn allow_code_kw(kwargs: &[(String, Object)], who: &str) -> Result<bool, RuntimeError> {
    let mut allow = true;
    for (k, v) in kwargs {
        if k == "allow_code" {
            allow = v.is_truthy();
        } else {
            return Err(type_error(format!(
                "{who}() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    Ok(allow)
}

/// The optional positional `version` argument of `dump`/`dumps`.
fn version_arg(arg: Option<&Object>) -> Result<i64, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(MARSHAL_VERSION),
        Some(Object::Int(v)) => Ok(*v),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(other) => Err(type_error(format!(
            "marshal version must be int, not {}",
            other.type_name()
        ))),
    }
}

fn dumps_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let value = args
        .first()
        .ok_or_else(|| type_error("dumps requires a value"))?;
    let version = version_arg(args.get(1))?;
    crate::stdlib::sys::audit_event("marshal.dumps", &[value.clone(), Object::Int(version)])?;
    let allow_code = allow_code_kw(kwargs, "dumps")?;
    let mut writer = MarshalWriter::new(version, allow_code);
    writer.write_value(value)?;
    Ok(Object::new_bytes(writer.into_bytes()))
}

fn loads_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let bytes = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("loads requires bytes-like"))?;
    crate::stdlib::sys::audit_event(
        "marshal.loads",
        std::slice::from_ref(args.first().expect("checked above")),
    )?;
    let allow_code = allow_code_kw(kwargs, "loads")?;
    let mut reader = MarshalReader::from_bytes(&bytes, allow_code);
    reader.read_value()
}

fn dump_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("dump() requires (value, file)"));
    }
    let version = version_arg(args.get(2))?;
    crate::stdlib::sys::audit_event("marshal.dumps", &[args[0].clone(), Object::Int(version)])?;
    let allow_code = allow_code_kw(kwargs, "dump")?;
    let mut writer = MarshalWriter::new(version, allow_code);
    writer.write_value(&args[0])?;
    let data = writer.into_bytes();
    match &args[1] {
        Object::File(f) => {
            f.write_bytes(&data)?;
            Ok(Object::None)
        }
        // Any file-like object: route through its `write` method (the
        // usual case is an `io.BytesIO`, test_no_allow_code).
        other => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("dump() expected a file-like object"))?;
            let vm = unsafe { &mut *ptr };
            let write = vm
                .load_attr_public(other, "write")
                .map_err(|_| type_error("marshal.dump() 2nd arg must have a write() method"))?;
            vm.call_object(write, &[Object::new_bytes(data)], &[])?;
            Ok(Object::None)
        }
    }
}

fn load_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let f = args
        .first()
        .ok_or_else(|| type_error("load() requires a file"))?;
    crate::stdlib::sys::audit_event("marshal.load", &[])?;
    let allow_code = allow_code_kw(kwargs, "load")?;
    let mut reader = MarshalReader::from_file(f.clone(), allow_code)?;
    reader.read_value()
}

/// Args-only entry kept for internal callers (`pycache`, frozen-code
/// cache). Writes the default version-5 form.
pub fn b_dumps(args: &[Object]) -> Result<Object, RuntimeError> {
    dumps_kw(args, &[])
}

/// Args-only entry kept for internal callers.
pub fn b_loads(args: &[Object]) -> Result<Object, RuntimeError> {
    loads_kw(args, &[])
}

// ---------- writer ----------

/// CPython `Python/marshal.c` `MAX_MARSHAL_STACK_DEPTH` (non-Windows):
/// both directions recurse natively per nesting level, so an unbounded
/// depth overflows the Rust stack and aborts. The guard converts a
/// deeply nested value into the `ValueError` CPython raises
/// (test_marshal `test_recursion_limit` / `test_loads_recursion`).
const MAX_MARSHAL_STACK_DEPTH: usize = 2000;

struct MarshalWriter {
    buf: Vec<u8>,
    depth: usize,
    version: i64,
    allow_code: bool,
    /// version ≥ 3: identity (`id()`) of every object already written,
    /// mapped to its index in the reader's reference vector. A repeat
    /// occurrence is written as `TYPE_REF idx` — this is what makes
    /// recursive containers marshallable and preserves aliasing
    /// (test_marshal.InstancingTestCase).
    refs: std::collections::HashMap<u64, u32>,
    /// Strong clones of every ref-registered object. Identity is the
    /// allocation address, so a registered temporary (e.g. the tuples
    /// `write_code` synthesizes) must stay alive for the whole dump or
    /// a later allocation could reuse its address and produce a bogus
    /// `TYPE_REF`.
    keepalive: Vec<Object>,
}

impl MarshalWriter {
    fn new(version: i64, allow_code: bool) -> Self {
        Self {
            buf: Vec::new(),
            depth: 0,
            version,
            allow_code,
            refs: std::collections::HashMap::new(),
            keepalive: Vec::new(),
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn write_byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn write_int(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_value(&mut self, value: &Object) -> Result<(), RuntimeError> {
        // CPython `w_object`: depth-guard every value, containers included.
        self.depth += 1;
        if self.depth > MAX_MARSHAL_STACK_DEPTH {
            self.depth -= 1;
            return Err(value_error("object too deeply nested to marshal"));
        }
        let r = self.write_dispatch(value);
        self.depth -= 1;
        r
    }

    fn write_dispatch(&mut self, value: &Object) -> Result<(), RuntimeError> {
        // Singletons sit outside the ref machinery (CPython `w_object`).
        match value {
            Object::None => {
                self.write_byte(TYPE_NONE);
                return Ok(());
            }
            Object::Bool(b) => {
                self.write_byte(if *b { TYPE_TRUE } else { TYPE_FALSE });
                return Ok(());
            }
            // The `StopIteration` *type* has its own wire code (CPython
            // uses it for the StopIteration ⇄ generator protocol;
            // test_marshal.ExceptionTestCase).
            Object::Type(t)
                if Rc::ptr_eq(t, &crate::builtin_types::builtin_types().stop_iteration) =>
            {
                self.write_byte(TYPE_STOPITER);
                return Ok(());
            }
            // `Ellipsis` (the value of `...`) is a singleton instance of
            // the registry `ellipsis` type.
            Object::Instance(inst)
                if Rc::ptr_eq(
                    &inst.cls(),
                    &crate::builtin_types::builtin_types().ellipsis_,
                ) =>
            {
                self.write_byte(TYPE_ELLIPSIS);
                return Ok(());
            }
            _ => {}
        }
        let mut flag = 0u8;
        if self.version >= 3 {
            let id = crate::weakref_registry::id_of(value);
            if let Some(&idx) = self.refs.get(&id) {
                self.write_byte(TYPE_REF);
                self.write_int(idx as i32);
                return Ok(());
            }
            // Register *before* the children are written so a recursive
            // container resolves to itself (CPython `w_ref`).
            let idx = self.refs.len() as u32;
            self.refs.insert(id, idx);
            self.keepalive.push(value.clone());
            flag = FLAG_REF;
        }
        self.write_body(value, flag)
    }

    fn write_body(&mut self, value: &Object, flag: u8) -> Result<(), RuntimeError> {
        match value {
            Object::Int(i) => {
                if let Ok(small) = i32::try_from(*i) {
                    self.write_byte(TYPE_INT | flag);
                    self.write_int(small);
                } else {
                    self.write_long_object(&BigInt::from(*i), flag)?;
                }
            }
            Object::Long(b) => self.write_long_object(b, flag)?,
            Object::Float(f) => {
                self.write_byte(TYPE_BINARY_FLOAT | flag);
                // Canonical NaN bits on the wire — never WeavePy's identity
                // tag (see `untag_nan`).
                self.buf
                    .extend_from_slice(&crate::object::untag_nan(*f).to_le_bytes());
            }
            Object::Complex(c) => {
                self.write_byte(TYPE_BINARY_COMPLEX | flag);
                self.buf
                    .extend_from_slice(&crate::object::untag_nan(c.real).to_le_bytes());
                self.buf
                    .extend_from_slice(&crate::object::untag_nan(c.imag).to_le_bytes());
            }
            Object::Str(s) => {
                // `sys.intern`ed strings keep their pooled identity across
                // the round-trip via the `*_INTERNED` codes — version ≥ 3
                // only, like CPython (testNoIntern dumps with version 2
                // and expects a fresh instance back).
                let interned = self.version >= 3 && crate::stdlib::sys::str_is_interned(value);
                let bytes = s.as_bytes();
                if bytes.is_ascii() && bytes.len() <= 255 {
                    self.write_byte(
                        if interned {
                            TYPE_SHORT_ASCII_INTERNED
                        } else {
                            TYPE_SHORT_ASCII
                        } | flag,
                    );
                    self.buf.push(bytes.len() as u8);
                    self.buf.extend_from_slice(bytes);
                } else if bytes.is_ascii() {
                    self.write_byte(
                        if interned {
                            TYPE_ASCII_INTERNED
                        } else {
                            TYPE_ASCII
                        } | flag,
                    );
                    self.write_int(bytes.len() as i32);
                    self.buf.extend_from_slice(bytes);
                } else {
                    self.write_byte(
                        if interned {
                            TYPE_INTERNED
                        } else {
                            TYPE_UNICODE
                        } | flag,
                    );
                    self.write_int(bytes.len() as i32);
                    self.buf.extend_from_slice(bytes);
                }
            }
            Object::WStr(cps) => {
                // A str carrying lone surrogates. CPython marshals unicode
                // through a surrogatepass UTF-8 encode, so the WTF-8 bytes
                // round-trip via `TYPE_UNICODE` (RFC 0033 parity).
                let bytes =
                    crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogatepass")?;
                self.write_byte(TYPE_UNICODE | flag);
                self.write_int(bytes.len() as i32);
                self.buf.extend_from_slice(&bytes);
            }
            Object::Bytes(data) => {
                self.write_byte(TYPE_STRING | flag);
                self.write_int(data.len() as i32);
                self.buf.extend_from_slice(data);
            }
            Object::ByteArray(data) => {
                let bytes = data.borrow();
                self.write_byte(TYPE_STRING | flag);
                self.write_int(bytes.len() as i32);
                self.buf.extend_from_slice(&bytes);
            }
            // Buffer-protocol values (memoryview, array.array, …) dump as
            // plain bytes, exactly like CPython's `w_object` buffer branch
            // (test_marshal.BufferTestCase).
            Object::MemoryView(mv) => {
                let bytes = mv.to_bytes();
                self.write_byte(TYPE_STRING | flag);
                self.write_int(bytes.len() as i32);
                self.buf.extend_from_slice(&bytes);
            }
            Object::Tuple(items) => {
                if items.len() < 256 {
                    self.write_byte(TYPE_SMALL_TUPLE | flag);
                    self.buf.push(items.len() as u8);
                } else {
                    self.write_byte(TYPE_TUPLE | flag);
                    self.write_int(items.len() as i32);
                }
                for item in items.iter() {
                    self.write_value(item)?;
                }
            }
            Object::List(items) => {
                let items = items.borrow();
                self.write_byte(TYPE_LIST | flag);
                self.write_int(items.len() as i32);
                for item in items.iter() {
                    self.write_value(item)?;
                }
            }
            Object::Dict(d) => {
                self.write_byte(TYPE_DICT | flag);
                let d = d.borrow();
                for (k, v) in d.iter() {
                    self.write_value(&k.0)?;
                    self.write_value(v)?;
                }
                self.write_byte(TYPE_NULL);
            }
            Object::Set(s) => {
                let s = s.borrow();
                self.write_byte(TYPE_SET | flag);
                self.write_int(s.len() as i32);
                for k in s.iter() {
                    self.write_value(&k.0)?;
                }
            }
            Object::FrozenSet(s) => {
                self.write_byte(TYPE_FROZENSET | flag);
                self.write_int(s.len() as i32);
                for k in s.iter() {
                    self.write_value(&k.0)?;
                }
            }
            Object::Code(co) => {
                if !self.allow_code {
                    return Err(value_error("unmarshallable object"));
                }
                self.write_code(co, flag)?;
            }
            // Slices are only representable from version 5 (CPython 3.14);
            // older versions reject them like any other unknown object.
            Object::Slice(s) if self.version >= 5 => {
                self.write_byte(TYPE_SLICE | flag);
                self.write_value(&s.start)?;
                self.write_value(&s.stop)?;
                self.write_value(&s.step)?;
            }
            other => {
                // Last resort: an instance exporting the buffer protocol
                // (PEP 688 `__buffer__`, e.g. `array.array`) dumps as bytes.
                if let Some(bytes) = instance_buffer_bytes(other) {
                    self.write_byte(TYPE_STRING | flag);
                    self.write_int(bytes.len() as i32);
                    self.buf.extend_from_slice(&bytes);
                    return Ok(());
                }
                // CPython's exact wording (test_unmarshallable matches it).
                let _ = other;
                return Err(value_error("unmarshallable object"));
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
    fn write_long_object(&mut self, b: &BigInt, flag: u8) -> Result<(), RuntimeError> {
        self.write_byte(TYPE_LONG | flag);
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
    fn write_code(&mut self, co: &CodeObject, flag: u8) -> Result<(), RuntimeError> {
        // A pool slot holding a value with no constant representation
        // (`code.replace(co_consts=(frozenset({int}),))`) marks the code
        // object unmarshallable, like CPython's `w_object` failing on
        // the underlying value (gh-106287).
        fn has_unmarshallable(consts: &[weavepy_compiler::Constant]) -> bool {
            use weavepy_compiler::Constant;
            consts.iter().any(|c| match c {
                Constant::Unmarshallable => true,
                Constant::Tuple(xs) | Constant::FrozenSet(xs) => has_unmarshallable(xs),
                Constant::Code(inner) => has_unmarshallable(&inner.constants),
                _ => false,
            })
        }
        if has_unmarshallable(&co.constants) {
            return Err(value_error("unmarshallable object"));
        }
        let cp = co.to_cpython();
        self.write_byte(TYPE_CODE | flag);
        self.write_int(co.arg_count as i32);
        self.write_int(co.posonly_count as i32);
        self.write_int(co.kwonly_count as i32);
        self.write_int(cp.stacksize as i32);
        self.write_int(code_flags(co) as i32);
        self.write_value(&Object::new_bytes(cp.co_code.clone()))?;
        let consts: Vec<Object> = co
            .constants
            .iter()
            .cloned()
            .map(crate::constant_to_object_public)
            .collect();
        self.write_value(&Object::new_tuple(consts))?;
        self.write_value(&strs_to_tuple(&co.names))?;
        self.write_value(&strs_to_tuple(&cp.localsplusnames))?;
        self.write_value(&Object::new_bytes(cp.localspluskinds.clone()))?;
        self.write_value(&Object::from_str(co.filename.clone()))?;
        self.write_value(&Object::from_str(co.name.clone()))?;
        // PEP 3155 qualified name, computed at compile time from lexical
        // nesting (`outer.<locals>.inner`, `C.method`). Round-trips so an
        // unmarshalled function/class keeps a faithful `__qualname__`.
        self.write_value(&Object::from_str(co.qualname.clone()))?;
        self.write_int(cp.firstlineno as i32);
        self.write_value(&Object::new_bytes(cp.co_linetable.clone()))?;
        self.write_value(&Object::new_bytes(cp.co_exceptiontable.clone()))?;
        Ok(())
    }
}

/// Buffer-protocol bytes of an arbitrary instance (PEP 688 `__buffer__`),
/// through the live interpreter. `None` when the value has no buffer or
/// no interpreter is active on this thread.
fn instance_buffer_bytes(value: &Object) -> Option<Vec<u8>> {
    if !matches!(value, Object::Instance(_)) {
        return None;
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    let vm = unsafe { &mut *ptr };
    let globals = vm.builtins_dict();
    match vm.memoryview_from_object_and_flags(value, 0x011C, &globals) {
        Ok(Some(mv)) => mv.as_bytes_view(),
        _ => None,
    }
}

/// CPython `co_flags` for a WeavePy code object (`CodeObject::co_flags`).
fn code_flags(co: &CodeObject) -> u32 {
    co.co_flags()
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

/// Byte source for the reader: an in-memory buffer (`loads`, `.pyc`
/// bodies) or a file-like object read incrementally through the
/// interpreter (`load`), which leaves the stream position exactly past
/// the value (test_multiple_dumps_and_loads).
enum MarshalSrc<'a> {
    Bytes {
        bytes: &'a [u8],
        pos: usize,
    },
    NativeFile {
        file: Rc<PyFile>,
    },
    File {
        file: Object,
        readinto: Option<Object>,
    },
}

struct MarshalReader<'a> {
    src: MarshalSrc<'a>,
    depth: usize,
    allow_code: bool,
    /// Objects registered by `FLAG_REF` in stream order; `None` marks a
    /// reserved slot whose object is still being built (a `TYPE_REF`
    /// into one is bad data).
    refs: Vec<Option<Object>>,
}

impl<'a> MarshalReader<'a> {
    fn from_bytes(bytes: &'a [u8], allow_code: bool) -> Self {
        Self {
            src: MarshalSrc::Bytes { bytes, pos: 0 },
            depth: 0,
            allow_code,
            refs: Vec::new(),
        }
    }

    fn from_file(f: Object, allow_code: bool) -> Result<Self, RuntimeError> {
        let src = match &f {
            Object::File(file) => MarshalSrc::NativeFile { file: file.clone() },
            other => {
                // Prefer `readinto` — CPython's `r_string` does, and
                // test_bad_reader instruments it to lie about the count.
                let readinto = crate::vm_singletons::current_interpreter_ptr().and_then(|ptr| {
                    let vm = unsafe { &mut *ptr };
                    vm.load_attr_public(other, "readinto").ok()
                });
                MarshalSrc::File {
                    file: f.clone(),
                    readinto,
                }
            }
        };
        Ok(Self {
            src,
            depth: 0,
            allow_code,
            refs: Vec::new(),
        })
    }

    /// CPython `marshal.c:r_string` raises EOFError("marshal data too
    /// short") — not ValueError — whenever a fixed-width read runs off
    /// the end of the buffer (test_importlib SourceLoaderBadBytecode
    /// `_test_bad_marshal` counts on the EOFError).
    fn truncated_error() -> RuntimeError {
        RuntimeError::PyException(crate::error::PyException::from_builtin(
            "EOFError",
            "marshal data too short",
        ))
    }

    /// Read exactly `n` bytes from the source. A stream that hands back
    /// *more* than requested is corrupt (CPython raises ValueError,
    /// test_bad_reader); one that hands back fewer is truncated.
    fn take(&mut self, n: usize) -> Result<Vec<u8>, RuntimeError> {
        match &mut self.src {
            MarshalSrc::Bytes { bytes, pos } => {
                if *pos + n > bytes.len() {
                    return Err(Self::truncated_error());
                }
                let out = bytes[*pos..*pos + n].to_vec();
                *pos += n;
                Ok(out)
            }
            MarshalSrc::NativeFile { file } => {
                let data = file.read_bytes(Some(n))?;
                if data.len() < n {
                    return Err(Self::truncated_error());
                }
                Ok(data)
            }
            MarshalSrc::File { file, readinto } => {
                let ptr = crate::vm_singletons::current_interpreter_ptr()
                    .ok_or_else(|| type_error("marshal.load() requires an active interpreter"))?;
                let vm = unsafe { &mut *ptr };
                if let Some(ri) = readinto.clone() {
                    let ba = Rc::new(RefCell::new(vec![0u8; n]));
                    let got = vm.call_object(ri, &[Object::ByteArray(ba.clone())], &[])?;
                    let m = match got {
                        Object::Int(m) if m >= 0 => m as usize,
                        Object::None => 0,
                        _ => return Err(value_error("readinto() returned an invalid length")),
                    };
                    if m > n {
                        return Err(value_error("read() returned too much data"));
                    }
                    if m < n {
                        return Err(Self::truncated_error());
                    }
                    return Ok(ba.borrow().clone());
                }
                let read = vm
                    .load_attr_public(file, "read")
                    .map_err(|_| type_error("marshal.load() arg must have a read() method"))?;
                let data = vm.call_object(read, &[Object::Int(n as i64)], &[])?;
                let bytes = data
                    .as_bytes_view()
                    .ok_or_else(|| type_error("read() returned non-bytes"))?;
                if bytes.len() > n {
                    return Err(value_error("read() returned too much data"));
                }
                if bytes.len() < n {
                    return Err(Self::truncated_error());
                }
                Ok(bytes)
            }
        }
    }

    fn read_byte(&mut self) -> Result<u8, RuntimeError> {
        match self.take(1) {
            Ok(b) => Ok(b[0]),
            // EOF at an object boundary is EOFError with CPython's
            // boundary message (test_exceptions.testRaising).
            Err(_) => Err(RuntimeError::PyException(
                crate::error::PyException::from_builtin(
                    "EOFError",
                    "EOF read where object expected",
                ),
            )),
        }
    }

    fn read_int(&mut self) -> Result<i32, RuntimeError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_long(&mut self) -> Result<i64, RuntimeError> {
        let b = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&b);
        Ok(i64::from_le_bytes(buf))
    }

    fn read_short(&mut self) -> Result<u16, RuntimeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_n_bytes(&mut self, n: usize) -> Result<Vec<u8>, RuntimeError> {
        self.take(n)
    }

    fn read_value(&mut self) -> Result<Object, RuntimeError> {
        self.read_value_opt()?
            .ok_or_else(|| value_error("bad marshal data (NULL object in marshal data)"))
    }

    /// Register `o` in the reference vector when its tag carried
    /// `FLAG_REF`.
    fn note_ref(&mut self, flag: bool, o: &Object) {
        if flag {
            self.refs.push(Some(o.clone()));
        }
    }

    /// Reserve a reference slot (containers register *before* their
    /// children so recursion resolves; tuples fill the slot after).
    fn reserve_ref(&mut self, flag: bool) -> Option<usize> {
        if flag {
            self.refs.push(None);
            Some(self.refs.len() - 1)
        } else {
            None
        }
    }

    fn fill_ref(&mut self, idx: Option<usize>, o: &Object) {
        if let Some(i) = idx {
            self.refs[i] = Some(o.clone());
        }
    }

    /// Read one value; `Ok(None)` is the `TYPE_NULL` sentinel that
    /// terminates dict bodies.
    fn read_value_opt(&mut self) -> Result<Option<Object>, RuntimeError> {
        // CPython `r_object`: depth-guard so malicious/deep input raises
        // instead of overflowing the native stack (test_loads_recursion).
        self.depth += 1;
        if self.depth > MAX_MARSHAL_STACK_DEPTH {
            self.depth -= 1;
            return Err(value_error("recursion limit exceeded"));
        }
        let r = self.read_value_inner();
        self.depth -= 1;
        r
    }

    fn read_value_inner(&mut self) -> Result<Option<Object>, RuntimeError> {
        let raw = self.read_byte()?;
        let flag = raw & FLAG_REF != 0;
        let tag = raw & !FLAG_REF;
        let obj = match tag {
            TYPE_NULL => return Ok(None),
            TYPE_NONE => Object::None,
            TYPE_TRUE => Object::Bool(true),
            TYPE_FALSE => Object::Bool(false),
            TYPE_ELLIPSIS => crate::vm_singletons::ellipsis(),
            TYPE_STOPITER => {
                Object::Type(crate::builtin_types::builtin_types().stop_iteration.clone())
            }
            TYPE_REF => {
                let idx = self.read_int()?;
                let resolved = usize::try_from(idx)
                    .ok()
                    .and_then(|i| self.refs.get(i).cloned())
                    .flatten();
                return match resolved {
                    Some(o) => Ok(Some(o)),
                    None => Err(value_error("bad marshal data (invalid reference)")),
                };
            }
            TYPE_INT => {
                let v = self.read_int()?;
                Object::Int(i64::from(v))
            }
            TYPE_INT64 => {
                let v = self.read_long()?;
                Object::Int(v)
            }
            TYPE_FLOAT => {
                let len = self.read_byte()? as usize;
                let bytes = self.read_n_bytes(len)?;
                let s =
                    std::str::from_utf8(&bytes).map_err(|_| value_error("bad marshal float"))?;
                crate::object::fresh_float(s.parse().unwrap_or(0.0))
            }
            TYPE_BINARY_FLOAT => {
                let bytes = self.read_n_bytes(8)?;
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&bytes);
                // Fresh identity for a canonical NaN; exotic payloads kept
                // verbatim (see `tag_unpacked_nan`).
                Object::Float(crate::object::tag_unpacked_nan(f64::from_le_bytes(buf)))
            }
            TYPE_BINARY_COMPLEX => {
                let real = self.read_n_bytes(8)?;
                let imag = self.read_n_bytes(8)?;
                let mut rb = [0u8; 8];
                rb.copy_from_slice(&real);
                let mut ib = [0u8; 8];
                ib.copy_from_slice(&imag);
                Object::Complex(Rc::new(PyComplex::new(
                    f64::from_le_bytes(rb),
                    f64::from_le_bytes(ib),
                )))
            }
            TYPE_LONG => {
                // Signed count of 15-bit little-endian digits (CPython
                // marshal). Reassemble as a `BigInt`, then auto-demote.
                let signed_count = self.read_int()?;
                let count = signed_count.unsigned_abs() as usize;
                let mut value = BigInt::from(0);
                let mut last_digit = 0u16;
                for i in 0..count {
                    let digit = self.read_short()?;
                    // 15-bit digits: anything larger is corrupt, and a
                    // zero top digit is an unnormalized long — both are
                    // ValueError in CPython (test_invalid_longs).
                    if digit > 0x7FFF {
                        return Err(value_error("bad marshal data (digit out of range in long)"));
                    }
                    last_digit = digit;
                    value += BigInt::from(digit) << (15 * i);
                }
                if count > 0 && last_digit == 0 {
                    return Err(value_error("bad marshal data (unnormalized long data)"));
                }
                if signed_count < 0 {
                    value = -value;
                }
                Object::int_from_bigint(value)
            }
            TYPE_CODE => {
                if !self.allow_code {
                    return Err(value_error(
                        "code objects are disallowed in restricted mode",
                    ));
                }
                let idx = self.reserve_ref(flag);
                let code = self.read_code()?;
                self.fill_ref(idx, &code);
                return Ok(Some(code));
            }
            TYPE_STRING => {
                let len = self.read_int()? as usize;
                let bytes = self.read_n_bytes(len)?;
                Object::new_bytes(bytes)
            }
            TYPE_UNICODE | TYPE_INTERNED | TYPE_ASCII | TYPE_ASCII_INTERNED => {
                let len = self.read_int()? as usize;
                let bytes = self.read_n_bytes(len)?;
                // CPython reads unicode with surrogatepass, so WTF-8 bytes
                // carrying lone surrogates rebuild an `Object::WStr`; pure
                // UTF-8 folds back to `Object::Str`.
                let s =
                    crate::stdlib::codecs_mod::decode_bytes_obj(&bytes, "utf-8", "surrogatepass")
                        .map_err(|_| value_error("bad marshal string"))?;
                if matches!(tag, TYPE_INTERNED | TYPE_ASCII_INTERNED) {
                    intern_loaded(s)
                } else {
                    s
                }
            }
            TYPE_SHORT_ASCII | TYPE_SHORT_ASCII_INTERNED => {
                let len = self.read_byte()? as usize;
                let bytes = self.read_n_bytes(len)?;
                let s =
                    String::from_utf8(bytes).map_err(|_| value_error("bad marshal short ascii"))?;
                if tag == TYPE_SHORT_ASCII_INTERNED {
                    crate::stdlib::sys::intern_name(&s)
                } else {
                    Object::from_str(s)
                }
            }
            TYPE_TUPLE | TYPE_SMALL_TUPLE => {
                let len = if tag == TYPE_TUPLE {
                    self.read_int()? as usize
                } else {
                    self.read_byte()? as usize
                };
                let idx = self.reserve_ref(flag);
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                let t = Object::new_tuple(items);
                self.fill_ref(idx, &t);
                return Ok(Some(t));
            }
            TYPE_SLICE => {
                let idx = self.reserve_ref(flag);
                let start = self.read_value()?;
                let stop = self.read_value()?;
                let step = self.read_value()?;
                let s = Object::Slice(Rc::new(crate::object::PySlice { start, stop, step }));
                self.fill_ref(idx, &s);
                return Ok(Some(s));
            }
            TYPE_LIST => {
                let len = self.read_int()? as usize;
                let list = Object::new_list(Vec::new());
                // Registered before the elements so a recursive list
                // resolves to itself (CPython `r_ref` for containers).
                self.note_ref(flag, &list);
                if let Object::List(l) = &list {
                    for _ in 0..len {
                        let item = self.read_value()?;
                        l.borrow_mut().push(item);
                    }
                }
                return Ok(Some(list));
            }
            TYPE_DICT => {
                let dict = Object::new_dict();
                self.note_ref(flag, &dict);
                if let Object::Dict(d) = &dict {
                    loop {
                        let Some(k) = self.read_value_opt()? else {
                            break;
                        };
                        let v = self.read_value()?;
                        d.borrow_mut().insert(DictKey(k), v);
                    }
                }
                return Ok(Some(dict));
            }
            TYPE_SET => {
                let len = self.read_int()? as usize;
                let set = Object::new_set_from(Vec::new());
                self.note_ref(flag, &set);
                if let Object::Set(s) = &set {
                    for _ in 0..len {
                        let item = self.read_value()?;
                        s.borrow_mut().insert(DictKey(item));
                    }
                }
                return Ok(Some(set));
            }
            TYPE_FROZENSET => {
                let len = self.read_int()? as usize;
                let idx = self.reserve_ref(flag);
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_value()?);
                }
                let fs = Object::new_frozenset_from(items);
                self.fill_ref(idx, &fs);
                return Ok(Some(fs));
            }
            other => return Err(value_error(format!("marshal: unknown type tag {other:?}"))),
        };
        self.note_ref(flag, &obj);
        Ok(Some(obj))
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

        let constants = tuple_to_constants(&consts)?;
        let decoded = cpython_code::decode_full(
            &code_bytes,
            &line_bytes,
            &exc_bytes,
            &lpn,
            &lpk,
            firstlineno,
            &constants,
        )
        .ok_or_else(|| value_error("marshal: code object uses an unsupported opcode"))?;

        let co_name = string_of(&name, "co_name")?;
        // Fall back to the bare name when the producer didn't record a
        // qualname (e.g. older marshal payloads); CPython always writes one.
        let co_qualname = string_of(&qualname, "co_qualname").unwrap_or_else(|_| co_name.clone());
        // Invert `code_flags`: only function scopes carry CO_OPTIMIZED,
        // so an unoptimized non-module body is a class body. Without
        // this a `.pyc` round-trip promoted class-body code to
        // CO_OPTIMIZED|CO_NEWLOCALS on `co_flags`
        // (test_capi.test_eval_code_ex test_custom_locals).
        let is_class_body = flags & CO_OPTIMIZED == 0 && co_name != "<module>";
        let co = CodeObject {
            name: co_name,
            qualname: co_qualname,
            filename: string_of(&filename, "co_filename")?,
            caches: CacheTable::with_len(decoded.instructions.len()),
            vm_ext: weavepy_compiler::VmExt::default(),
            jit_hint: weavepy_compiler::JitHint::default(),
            instructions: decoded.instructions,
            constants,
            names: tuple_of_strings(&names, "co_names")?,
            varnames: decoded.varnames,
            freevars: decoded.freevars,
            cellvars: decoded.cellvars,
            exception_table: decoded.exception_table,
            linetable: decoded.linetable,
            // PEP-657 columns recovered from long-form location entries
            // (RFC 0056 WS4): traceback caret underlines survive the
            // `.pyc` round-trip.
            coltable: decoded.coltable,
            arg_count,
            posonly_count,
            kwonly_count,
            has_varargs: flags & CO_VARARGS != 0,
            has_varkeywords: flags & CO_VARKEYWORDS != 0,
            is_class_body,
            is_generator: flags & CO_GENERATOR != 0,
            is_coroutine: flags & CO_COROUTINE != 0,
            is_async_generator: flags & CO_ASYNC_GENERATOR != 0,
            is_iterable_coroutine: flags & CO_ITERABLE_COROUTINE != 0,
            has_docstring: flags & CO_HAS_DOCSTRING != 0,
            is_method: flags & CO_METHOD != 0,
            is_nested: flags & CO_NESTED != 0,
            future_flags: flags & weavepy_compiler::flags::PYCF_MASK,
            cp_cache: cpython_code::CpCache::default(),
            wire: None,
            no_interrupt_jumps: decoded.no_interrupt_jumps,
            wire_marks: decoded.wire_marks,
            hidden_locals: decoded.hidden_locals,
        };
        Ok(Object::Code(Rc::new(co)))
    }
}

/// Intern a loaded string when it decoded to a plain `str`; `WStr`
/// (lone-surrogate) payloads pass through untouched.
fn intern_loaded(s: Object) -> Object {
    match &s {
        Object::Str(_) => crate::stdlib::sys::intern_name(&s.to_str()),
        _ => s,
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
    let mut w = MarshalWriter::new(MARSHAL_VERSION, true);
    w.write_value(value)?;
    file.write_bytes(&w.into_bytes())?;
    Ok(())
}

pub fn load_from_bytes(bytes: &[u8]) -> Result<Object, RuntimeError> {
    let mut r = MarshalReader::from_bytes(bytes, true);
    r.read_value()
}

#[allow(dead_code)]
fn discard_file(_: FileBackend) {}
