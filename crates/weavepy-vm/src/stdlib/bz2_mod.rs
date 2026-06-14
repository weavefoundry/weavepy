//! `_bz2` — bzip2 compress/decompress (RFC 0019; streaming classes RFC 0038).
//!
//! Backed by the `bzip2` crate (statically-linked libbz2). Exposes the
//! one-shot `compress`/`decompress` plus the incremental
//! `BZ2Compressor`/`BZ2Decompressor` classes that CPython's frozen
//! `bz2.py` builds the `BZ2File` wrapper on top of.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::{Action, Compress, Compression, Decompress, Error as BzError, Status};

use crate::error::{os_error, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_bz2"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("bzip2 compress/decompress (RFC 0019 core)."),
        );
        register(&mut d, "compress", b_compress);
        register(&mut d, "decompress", b_decompress);
        d.insert(
            DictKey(Object::from_static("BZ2Compressor")),
            Object::Type(compressor_class()),
        );
        d.insert(
            DictKey(Object::from_static("BZ2Decompressor")),
            Object::Type(decompressor_class()),
        );
    }
    Rc::new(PyModule {
        name: "_bz2".to_owned(),
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

fn b_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("compress requires bytes-like"))?;
    let level = args
        .get(1)
        .and_then(|o| o.as_i64())
        .unwrap_or(9)
        .clamp(1, 9) as u32;
    let mut enc = BzEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(&data)
        .map_err(|e| value_error(format!("bz2 compress: {e}")))?;
    let bytes = enc
        .finish()
        .map_err(|e| value_error(format!("bz2 compress: {e}")))?;
    Ok(Object::new_bytes(bytes))
}

fn b_decompress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("decompress requires bytes-like"))?;
    let mut dec = BzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| value_error(format!("bz2 decompress: {e}")))?;
    Ok(Object::new_bytes(out))
}

// ---------------------------------------------------------------------------
// Incremental streaming objects — `BZ2Compressor` / `BZ2Decompressor`.
//
// State lives in a thread-local registry keyed by an integer handle stored on
// the Python instance's `_handle`, mirroring `zlib.compressobj`. The classes
// are real (builtin-flagged) types so `isinstance`/`type()` behave and a
// Rust-implemented `__init__` allocates the engine.
// ---------------------------------------------------------------------------

struct BzCompState {
    c: Compress,
    done: bool,
}

struct BzDecompState {
    d: Decompress,
    /// Compressed input buffered across calls (when `max_length` capped the
    /// output and left unread input behind).
    input: Vec<u8>,
    eof: bool,
    needs_input: bool,
    unused_data: Vec<u8>,
}

// SAFETY: WeavePy serialises all bytecode execution (and therefore every
// touch of this libbz2 stream state) behind the process-wide GIL — only one
// OS thread runs Python at a time. The registry `Mutex` adds the memory
// barrier when a stream object created on one thread is used from another
// (CPython `BZ2File` is explicitly shared across threads in test_bz2's
// `testThreading`). The raw `bz_stream` pointers are never touched
// concurrently, so promising `Send` is sound.
unsafe impl Send for BzCompState {}
unsafe impl Send for BzDecompState {}

/// Process-global registries keyed by the integer handle stored on each
/// Python instance. Global (not thread-local) so a compressor/decompressor
/// created on one thread is reachable from any other (see `testThreading`).
type CompReg = Mutex<HashMap<i64, Rc<RefCell<BzCompState>>>>;
type DecompReg = Mutex<HashMap<i64, Rc<RefCell<BzDecompState>>>>;

fn comp_reg() -> &'static CompReg {
    static REG: OnceLock<CompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn decomp_reg() -> &'static DecompReg {
    static REG: OnceLock<DecompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn bz_next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn comp_state(id: i64) -> Option<Rc<RefCell<BzCompState>>> {
    comp_reg().lock().ok()?.get(&id).cloned()
}

fn decomp_state(id: i64) -> Option<Rc<RefCell<BzDecompState>>> {
    decomp_reg().lock().ok()?.get(&id).cloned()
}

fn eof_error(msg: &str) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("EOFError", msg))
}

fn bytes_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(o) => o
            .as_bytes_view()
            .ok_or_else(|| type_error("a bytes-like object is required")),
        None => Err(type_error("a bytes-like object is required")),
    }
}

fn handle_of(args: &[Object]) -> Result<i64, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i,
        _ => return Err(type_error("expected bz2 compressor/decompressor object")),
    };
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
    {
        Some(Object::Int(v)) => Ok(v),
        _ => Err(type_error("bz2 object missing _handle")),
    }
}

/// Drive `Compress::compress` to completion for one call. `Action::Run`
/// feeds data; `Action::Finish` finalises the stream.
fn bz_compress_step(
    c: &mut Compress,
    input: &[u8],
    action: Action,
) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut consumed = 0usize;
    loop {
        let before_in = c.total_in();
        let before_out = c.total_out();
        let status = c
            .compress(&input[consumed..], &mut buf, action)
            .map_err(|e| value_error(format!("bz2 compress error: {e:?}")))?;
        let din = (c.total_in() - before_in) as usize;
        let dout = (c.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        if matches!(status, Status::StreamEnd) {
            break;
        }
        let finishing = matches!(action, Action::Finish);
        // A short write while running/flushing means the action is done; when
        // finishing we keep going until `StreamEnd`. The no-progress guard is
        // a safety net against spinning.
        if !finishing && dout < buf.len() {
            break;
        }
        if din == 0 && dout == 0 {
            break;
        }
    }
    Ok(out)
}

/// Drive `Decompress::decompress` for one call. `limit` caps the produced
/// output (`max_length`); `None` means unbounded. Returns
/// `(output, input_consumed, stream_end)`.
fn bz_decompress_step(
    d: &mut Decompress,
    input: &[u8],
    limit: Option<usize>,
) -> Result<(Vec<u8>, usize, bool), RuntimeError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut consumed = 0usize;
    let mut eof = false;
    loop {
        let room = match limit {
            Some(l) => {
                if out.len() >= l {
                    break;
                }
                (l - out.len()).min(buf.len())
            }
            None => buf.len(),
        };
        let before_in = d.total_in();
        let before_out = d.total_out();
        let status = d
            .decompress(&input[consumed..], &mut buf[..room])
            .map_err(|e| match e {
                BzError::Sequence => os_error("bz2: invalid sequence of operations"),
                _ => os_error("Invalid data stream"),
            })?;
        let din = (d.total_in() - before_in) as usize;
        let dout = (d.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        if matches!(status, Status::StreamEnd) {
            eof = true;
            break;
        }
        if din == 0 && dout == 0 {
            break;
        }
    }
    Ok((out, consumed, eof))
}

fn class_method(
    dict: &mut DictData,
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

fn class_method_kw(
    dict: &mut DictData,
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) {
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    );
}

fn compressor_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        class_method_kw(&mut dict, "__init__", compressor_init);
        class_method(&mut dict, "compress", compressor_compress);
        class_method(&mut dict, "flush", compressor_flush);
        class_method(&mut dict, "__reduce__", bz_no_pickle);
        class_method(&mut dict, "__reduce_ex__", bz_no_pickle);
        class_method(&mut dict, "__getstate__", bz_no_pickle);
        TypeObject::new_with_flags(
            "BZ2Compressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("BZ2Compressor must linearise")
    })
    .clone()
}

fn decompressor_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        class_method_kw(&mut dict, "__init__", decompressor_init);
        class_method_kw(&mut dict, "decompress", decompressor_decompress);
        class_method(&mut dict, "__reduce__", bz_no_pickle);
        class_method(&mut dict, "__reduce_ex__", bz_no_pickle);
        class_method(&mut dict, "__getstate__", bz_no_pickle);
        TypeObject::new_with_flags(
            "BZ2Decompressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("BZ2Decompressor must linearise")
    })
    .clone()
}

/// `__reduce__`/`__getstate__` for the streaming objects — they hold live
/// libbz2 state and cannot be pickled (matches CPython, which raises
/// `TypeError`).
fn bz_no_pickle(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Instance(i)) => i.cls().name.clone(),
        _ => "bz2 object".to_owned(),
    };
    Err(type_error(format!("cannot pickle '{name}' object")))
}

fn self_instance(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("method requires a bz2 object instance")),
    }
}

fn compressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let level = args
        .get(1)
        .and_then(Object::as_i64)
        .or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == "compresslevel")
                .and_then(|(_, v)| v.as_i64())
        })
        .unwrap_or(9);
    if !(1..=9).contains(&level) {
        return Err(value_error("compresslevel must be between 1 and 9"));
    }
    let id = bz_next_id();
    if let Ok(mut reg) = comp_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(BzCompState {
                c: Compress::new(Compression::new(level as u32), 0),
                done: false,
            })),
        );
    }
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    Ok(Object::None)
}

fn compressor_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = bytes_arg(args.get(1))?;
    let state = comp_state(id).ok_or_else(|| value_error("stale BZ2Compressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("Compressor has been flushed"));
    }
    let out = bz_compress_step(&mut st.c, &data, Action::Run)?;
    Ok(Object::new_bytes(out))
}

fn compressor_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let state = comp_state(id).ok_or_else(|| value_error("stale BZ2Compressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("Repeated call to flush()"));
    }
    let out = bz_compress_step(&mut st.c, &[], Action::Finish)?;
    st.done = true;
    Ok(Object::new_bytes(out))
}

fn decompressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    // CPython `BZ2Decompressor()` accepts no arguments.
    if args.len() > 1 || !kwargs.is_empty() {
        return Err(type_error("BZ2Decompressor() takes no arguments"));
    }
    let id = bz_next_id();
    if let Ok(mut reg) = decomp_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(BzDecompState {
                d: Decompress::new(false),
                input: Vec::new(),
                eof: false,
                needs_input: true,
                unused_data: Vec::new(),
            })),
        );
    }
    let mut d = inst.dict.borrow_mut();
    d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    d.insert(DictKey(Object::from_static("eof")), Object::Bool(false));
    d.insert(
        DictKey(Object::from_static("needs_input")),
        Object::Bool(true),
    );
    d.insert(
        DictKey(Object::from_static("unused_data")),
        Object::new_bytes(Vec::new()),
    );
    Ok(Object::None)
}

fn decompressor_decompress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = bytes_arg(args.get(1))?;
    let max_length = if let Some(o) = args.get(2) {
        crate::builtins::coerce_index_i64(o)?
    } else if let Some((_, o)) = kwargs.iter().find(|(k, _)| k == "max_length") {
        crate::builtins::coerce_index_i64(o)?
    } else {
        -1
    };
    let state = decomp_state(id).ok_or_else(|| value_error("stale BZ2Decompressor"))?;
    let mut st = state.borrow_mut();
    if st.eof {
        return Err(eof_error("End of stream already reached"));
    }
    let mut combined = std::mem::take(&mut st.input);
    combined.extend_from_slice(&data);
    let limit = if max_length < 0 {
        None
    } else {
        Some(max_length as usize)
    };
    let (out, consumed, eof) = bz_decompress_step(&mut st.d, &combined, limit)?;
    let leftover = combined[consumed..].to_vec();
    if eof {
        st.eof = true;
        st.needs_input = false;
        st.unused_data = leftover;
        st.input = Vec::new();
    } else {
        st.needs_input = leftover.is_empty();
        st.input = leftover;
    }
    if let Some(Object::Instance(inst)) = args.first() {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(st.eof));
        d.insert(
            DictKey(Object::from_static("needs_input")),
            Object::Bool(st.needs_input),
        );
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(st.unused_data.clone()),
        );
    }
    Ok(Object::new_bytes(out))
}
