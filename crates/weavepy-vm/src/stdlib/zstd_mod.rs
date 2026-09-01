//! `_zstd` — Zstandard bindings (PEP 784; RFC 0076 WS15).
//!
//! The native core under the verbatim CPython 3.14 `compression.zstd`
//! package (`python/compression/zstd/`). Backed by libzstd through
//! `zstd-sys` (vendored/static, matching the bzip2/lzma posture): the
//! streaming engine is `ZSTD_compressStream2` / `ZSTD_decompressStream`
//! driven exactly the way CPython's `Modules/_zstd` drives it, so the
//! `mode=`/`max_length=` semantics and the frame-boundary behaviour
//! (one `ZstdDecompressor` per frame, `unused_data` after `eof`) are
//! the documented ones.
//!
//! State lives in a process-global registry keyed by the integer
//! handle stored on each instance's `_handle` — the `_bz2`/`zlib`
//! streaming-object pattern.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

// ---------------------------------------------------------------------------
// ZstdError
// ---------------------------------------------------------------------------

/// `_zstd.ZstdError` — an `Exception` subclass, as CPython creates it.
fn zstd_error_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        TypeObject::new_with_flags(
            "ZstdError",
            vec![bt.exception.clone()],
            DictData::default(),
            TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("ZstdError must linearise")
    })
    .clone()
}

fn zstd_error(msg: impl Into<String>) -> RuntimeError {
    let inst = crate::builtin_types::make_exception_with_class(zstd_error_class(), msg.into());
    RuntimeError::PyException(PyException::new(inst))
}

fn eof_error(msg: &str) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("EOFError", msg))
}

/// Map a libzstd error code to `ZstdError(name)`.
fn check_zstd(code: usize, what: &str) -> Result<usize, RuntimeError> {
    if unsafe { zstd_sys::ZSTD_isError(code) } != 0 {
        let name = unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZSTD_getErrorName(code)) }
            .to_string_lossy()
            .into_owned();
        return Err(zstd_error(format!("{what}: {name}")));
    }
    Ok(code)
}

// ---------------------------------------------------------------------------
// Native engine state
// ---------------------------------------------------------------------------

struct CState {
    cctx: *mut zstd_sys::ZSTD_CCtx,
    last_mode: i64,
}

struct DState {
    dctx: *mut zstd_sys::ZSTD_DCtx,
    /// Unconsumed compressed input carried across calls.
    input: Vec<u8>,
    eof: bool,
    needs_input: bool,
    unused_data: Vec<u8>,
}

impl Drop for CState {
    fn drop(&mut self) {
        unsafe { zstd_sys::ZSTD_freeCCtx(self.cctx) };
    }
}

impl Drop for DState {
    fn drop(&mut self) {
        unsafe { zstd_sys::ZSTD_freeDCtx(self.dctx) };
    }
}

// SAFETY: bytecode execution is serialised behind the GIL; the registry
// mutex provides the memory barrier when an object created on one
// thread is used from another. The raw contexts are never touched
// concurrently (same contract as `_bz2`'s streams).
unsafe impl Send for CState {}
unsafe impl Send for DState {}

type CompReg = Mutex<HashMap<i64, Rc<RefCell<CState>>>>;
type DecompReg = Mutex<HashMap<i64, Rc<RefCell<DState>>>>;

fn comp_reg() -> &'static CompReg {
    static REG: OnceLock<CompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn decomp_reg() -> &'static DecompReg {
    static REG: OnceLock<DecompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn handle_of(args: &[Object]) -> Result<i64, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i,
        _ => return Err(type_error("expected a zstd compressor/decompressor object")),
    };
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
    {
        Some(Object::Int(v)) => Ok(v),
        _ => Err(type_error("zstd object missing _handle")),
    }
}

fn self_instance(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("method requires a zstd object instance")),
    }
}

fn kwarg<'a>(kwargs: &'a [(String, Object)], name: &str) -> Option<&'a Object> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn bytes_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(o) => o
            .as_bytes_view()
            .ok_or_else(|| type_error("a bytes-like object is required")),
        None => Err(type_error("a bytes-like object is required")),
    }
}

// ---------------------------------------------------------------------------
// Streaming engine
// ---------------------------------------------------------------------------

/// One `compress(data, mode)` / `flush(mode)` call: drive
/// `ZSTD_compressStream2` until the input is consumed and — for the
/// flush directives — the internal buffers report empty.
fn compress_step(
    cctx: *mut zstd_sys::ZSTD_CCtx,
    input: &[u8],
    end_op: zstd_sys::ZSTD_EndDirective,
) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::new();
    let chunk = unsafe { zstd_sys::ZSTD_CStreamOutSize() };
    let mut buf = vec![0u8; chunk];
    let mut in_buf = zstd_sys::ZSTD_inBuffer {
        src: input.as_ptr().cast::<c_void>(),
        size: input.len(),
        pos: 0,
    };
    loop {
        let mut out_buf = zstd_sys::ZSTD_outBuffer {
            dst: buf.as_mut_ptr().cast::<c_void>(),
            size: buf.len(),
            pos: 0,
        };
        let remaining = check_zstd(
            unsafe {
                zstd_sys::ZSTD_compressStream2(cctx, &raw mut out_buf, &raw mut in_buf, end_op)
            },
            "compress",
        )?;
        out.extend_from_slice(&buf[..out_buf.pos]);
        let input_done = in_buf.pos == in_buf.size;
        let flushed = remaining == 0;
        let continuing = matches!(end_op, zstd_sys::ZSTD_EndDirective::ZSTD_e_continue);
        if input_done && (continuing || flushed) {
            break;
        }
    }
    Ok(out)
}

/// One `decompress(data, max_length)` call over a single frame.
/// Returns `(output, input_consumed, frame_complete)`.
fn decompress_step(
    dctx: *mut zstd_sys::ZSTD_DCtx,
    input: &[u8],
    limit: Option<usize>,
) -> Result<(Vec<u8>, usize, bool), RuntimeError> {
    let mut out = Vec::new();
    let chunk = unsafe { zstd_sys::ZSTD_DStreamOutSize() };
    let mut in_buf = zstd_sys::ZSTD_inBuffer {
        src: input.as_ptr().cast::<c_void>(),
        size: input.len(),
        pos: 0,
    };
    let mut frame_end = false;
    let mut buf = vec![0u8; chunk];
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
        let mut out_buf = zstd_sys::ZSTD_outBuffer {
            dst: buf.as_mut_ptr().cast::<c_void>(),
            size: room,
            pos: 0,
        };
        let ret = check_zstd(
            unsafe { zstd_sys::ZSTD_decompressStream(dctx, &raw mut out_buf, &raw mut in_buf) },
            "decompress",
        )?;
        out.extend_from_slice(&buf[..out_buf.pos]);
        if ret == 0 {
            // Frame complete; anything left in `in_buf` is trailing data.
            frame_end = true;
            break;
        }
        if in_buf.pos == in_buf.size && out_buf.pos < out_buf.size {
            // Input exhausted mid-frame: need more data.
            break;
        }
    }
    Ok((out, in_buf.pos, frame_end))
}

// ---------------------------------------------------------------------------
// Options / dictionary plumbing
// ---------------------------------------------------------------------------

/// A raw compression-parameter code from Python, mapped onto the
/// libzstd enum. Unknown codes are rejected here (never transmuted:
/// an invalid discriminant would be UB).
fn cparam_from(code: i64) -> Result<zstd_sys::ZSTD_cParameter, RuntimeError> {
    use zstd_sys::ZSTD_cParameter as C;
    Ok(match code {
        100 => C::ZSTD_c_compressionLevel,
        101 => C::ZSTD_c_windowLog,
        102 => C::ZSTD_c_hashLog,
        103 => C::ZSTD_c_chainLog,
        104 => C::ZSTD_c_searchLog,
        105 => C::ZSTD_c_minMatch,
        106 => C::ZSTD_c_targetLength,
        107 => C::ZSTD_c_strategy,
        160 => C::ZSTD_c_enableLongDistanceMatching,
        161 => C::ZSTD_c_ldmHashLog,
        162 => C::ZSTD_c_ldmMinMatch,
        163 => C::ZSTD_c_ldmBucketSizeLog,
        164 => C::ZSTD_c_ldmHashRateLog,
        200 => C::ZSTD_c_contentSizeFlag,
        201 => C::ZSTD_c_checksumFlag,
        202 => C::ZSTD_c_dictIDFlag,
        400 => C::ZSTD_c_nbWorkers,
        401 => C::ZSTD_c_jobSize,
        402 => C::ZSTD_c_overlapLog,
        _ => return Err(zstd_error(format!("invalid compression parameter: {code}"))),
    })
}

fn dparam_from(code: i64) -> Result<zstd_sys::ZSTD_dParameter, RuntimeError> {
    use zstd_sys::ZSTD_dParameter as D;
    Ok(match code {
        100 => D::ZSTD_d_windowLogMax,
        _ => {
            return Err(zstd_error(format!(
                "invalid decompression parameter: {code}"
            )))
        }
    })
}

/// Apply an `options` dict of `{parameter_code: value}` (keys/values
/// coerced via `__index__`, so `CompressionParameter` IntEnum members
/// work directly).
fn apply_coptions(cctx: *mut zstd_sys::ZSTD_CCtx, options: &Object) -> Result<(), RuntimeError> {
    let Object::Dict(d) = options else {
        return Err(type_error("options must be a dict or None"));
    };
    let items: Vec<(Object, Object)> = d
        .borrow()
        .iter()
        .map(|(k, v)| (k.0.clone(), v.clone()))
        .collect();
    for (k, v) in items {
        let code = crate::builtins::coerce_index_i64(&k)?;
        let value = crate::builtins::coerce_index_i64(&v)? as c_int;
        check_zstd(
            unsafe { zstd_sys::ZSTD_CCtx_setParameter(cctx, cparam_from(code)?, value) },
            "invalid compression option",
        )?;
    }
    Ok(())
}

fn apply_doptions(dctx: *mut zstd_sys::ZSTD_DCtx, options: &Object) -> Result<(), RuntimeError> {
    let Object::Dict(d) = options else {
        return Err(type_error("options must be a dict or None"));
    };
    let items: Vec<(Object, Object)> = d
        .borrow()
        .iter()
        .map(|(k, v)| (k.0.clone(), v.clone()))
        .collect();
    for (k, v) in items {
        let code = crate::builtins::coerce_index_i64(&k)?;
        let value = crate::builtins::coerce_index_i64(&v)? as c_int;
        check_zstd(
            unsafe { zstd_sys::ZSTD_DCtx_setParameter(dctx, dparam_from(code)?, value) },
            "invalid decompression option",
        )?;
    }
    Ok(())
}

/// Resolve a `zstd_dict=` argument — a `ZstdDict` instance or the
/// `(ZstdDict, mode)` tuples its `as_digested_dict` / `as_undigested_dict`
/// / `as_prefix` accessors return. Yields `(dict_content, is_prefix)`.
fn dict_arg(obj: &Object) -> Result<Option<(Vec<u8>, bool)>, RuntimeError> {
    let (inst_obj, mode) = match obj {
        Object::None => return Ok(None),
        Object::Tuple(items) if items.len() == 2 => {
            let mode = items[1]
                .as_i64()
                .ok_or_else(|| type_error("invalid zstd_dict tuple"))?;
            (items[0].clone(), mode)
        }
        other => (other.clone(), 1),
    };
    let Object::Instance(inst) = &inst_obj else {
        return Err(type_error(
            "zstd_dict argument should be a ZstdDict object.",
        ));
    };
    let content = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("dict_content")))
        .cloned()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("zstd_dict argument should be a ZstdDict object."))?;
    Ok(Some((content, mode == 3)))
}

// ---------------------------------------------------------------------------
// ZstdCompressor
// ---------------------------------------------------------------------------

const MODE_CONTINUE: i64 = 0;
const MODE_FLUSH_BLOCK: i64 = 1;
const MODE_FLUSH_FRAME: i64 = 2;

fn end_directive(mode: i64) -> Result<zstd_sys::ZSTD_EndDirective, RuntimeError> {
    Ok(match mode {
        MODE_CONTINUE => zstd_sys::ZSTD_EndDirective::ZSTD_e_continue,
        MODE_FLUSH_BLOCK => zstd_sys::ZSTD_EndDirective::ZSTD_e_flush,
        MODE_FLUSH_FRAME => zstd_sys::ZSTD_EndDirective::ZSTD_e_end,
        _ => {
            return Err(value_error(
                "mode argument wrong value, it should be one of ZstdCompressor.CONTINUE, ZstdCompressor.FLUSH_BLOCK, ZstdCompressor.FLUSH_FRAME.",
            ))
        }
    })
}

fn compressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let level = args
        .get(1)
        .cloned()
        .or_else(|| kwarg(kwargs, "level").cloned());
    let options = args
        .get(2)
        .cloned()
        .or_else(|| kwarg(kwargs, "options").cloned());
    let zdict = args
        .get(3)
        .cloned()
        .or_else(|| kwarg(kwargs, "zstd_dict").cloned());

    let cctx = unsafe { zstd_sys::ZSTD_createCCtx() };
    if cctx.is_null() {
        return Err(zstd_error("unable to create a ZSTD_CCtx"));
    }
    let state = CState {
        cctx,
        last_mode: MODE_FLUSH_FRAME,
    };
    if let Some(level_obj) = &level {
        if !matches!(level_obj, Object::None) {
            let Some(level) = level_obj.as_i64() else {
                return Err(type_error("level must be int or None"));
            };
            check_zstd(
                unsafe {
                    zstd_sys::ZSTD_CCtx_setParameter(
                        cctx,
                        zstd_sys::ZSTD_cParameter::ZSTD_c_compressionLevel,
                        level as c_int,
                    )
                },
                "invalid compression level",
            )?;
        }
    }
    if let Some(opts) = &options {
        if !matches!(opts, Object::None) {
            apply_coptions(cctx, opts)?;
        }
    }
    if let Some(zd) = &zdict {
        if let Some((content, is_prefix)) = dict_arg(zd)? {
            if is_prefix {
                check_zstd(
                    unsafe {
                        zstd_sys::ZSTD_CCtx_refPrefix(
                            cctx,
                            content.as_ptr().cast::<c_void>(),
                            content.len(),
                        )
                    },
                    "unable to reference prefix",
                )?;
                // The prefix bytes must outlive the ref: stash them on
                // the instance so the borrow can't dangle.
                inst.dict.borrow_mut().insert(
                    DictKey(Object::from_static("_prefix_keepalive")),
                    Object::new_bytes(content),
                );
            } else {
                check_zstd(
                    unsafe {
                        zstd_sys::ZSTD_CCtx_loadDictionary(
                            cctx,
                            content.as_ptr().cast::<c_void>(),
                            content.len(),
                        )
                    },
                    "unable to load dictionary",
                )?;
            }
        }
    }
    let id = next_id();
    if let Ok(mut reg) = comp_reg().lock() {
        reg.insert(id, Rc::new(RefCell::new(state)));
    }
    let mut d = inst.dict.borrow_mut();
    d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    d.insert(
        DictKey(Object::from_static("last_mode")),
        Object::Int(MODE_FLUSH_FRAME),
    );
    Ok(Object::None)
}

fn comp_state(id: i64) -> Result<Rc<RefCell<CState>>, RuntimeError> {
    comp_reg()
        .lock()
        .ok()
        .and_then(|reg| reg.get(&id).cloned())
        .ok_or_else(|| value_error("stale ZstdCompressor"))
}

fn note_last_mode(args: &[Object], st: &mut CState, mode: i64) {
    st.last_mode = mode;
    if let Some(Object::Instance(inst)) = args.first() {
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("last_mode")), Object::Int(mode));
    }
}

fn compressor_compress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = bytes_arg(args.get(1))?;
    let mode = match args.get(2).or_else(|| kwarg(kwargs, "mode")) {
        Some(o) => crate::builtins::coerce_index_i64(o)?,
        None => MODE_CONTINUE,
    };
    let dir = end_directive(mode)?;
    let state = comp_state(id)?;
    let mut st = state.borrow_mut();
    let out = compress_step(st.cctx, &data, dir)?;
    note_last_mode(args, &mut st, mode);
    Ok(Object::new_bytes(out))
}

fn compressor_flush(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let mode = match args.get(1).or_else(|| kwarg(kwargs, "mode")) {
        Some(o) => crate::builtins::coerce_index_i64(o)?,
        None => MODE_FLUSH_FRAME,
    };
    if mode != MODE_FLUSH_BLOCK && mode != MODE_FLUSH_FRAME {
        return Err(value_error(
            "mode argument wrong value, it should be ZstdCompressor.FLUSH_FRAME or ZstdCompressor.FLUSH_BLOCK.",
        ));
    }
    let dir = end_directive(mode)?;
    let state = comp_state(id)?;
    let mut st = state.borrow_mut();
    let out = compress_step(st.cctx, &[], dir)?;
    note_last_mode(args, &mut st, mode);
    Ok(Object::new_bytes(out))
}

fn compressor_set_pledged_input_size(
    args: &[Object],
    _kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let state = comp_state(id)?;
    let st = state.borrow_mut();
    if st.last_mode != MODE_FLUSH_FRAME {
        return Err(value_error(
            "set_pledged_input_size() method must be called when last_mode == FLUSH_FRAME",
        ));
    }
    let size = match args.get(1) {
        None | Some(Object::None) => u64::MAX, // ZSTD_CONTENTSIZE_UNKNOWN
        Some(o) => {
            let v = crate::builtins::coerce_index_i64(o)?;
            if v < 0 {
                return Err(value_error(
                    "size argument should be a positive int less than 2**64",
                ));
            }
            v as u64
        }
    };
    check_zstd(
        unsafe { zstd_sys::ZSTD_CCtx_setPledgedSrcSize(st.cctx, size) },
        "set_pledged_input_size failed",
    )?;
    Ok(Object::None)
}

fn no_pickle(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Instance(i)) => i.cls().name.clone(),
        _ => "zstd object".to_owned(),
    };
    Err(type_error(format!("cannot pickle '{name}' object")))
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
        let mut dict = DictData::default();
        class_method_kw(&mut dict, "__init__", compressor_init);
        class_method_kw(&mut dict, "compress", compressor_compress);
        class_method_kw(&mut dict, "flush", compressor_flush);
        class_method_kw(
            &mut dict,
            "set_pledged_input_size",
            compressor_set_pledged_input_size,
        );
        class_method(&mut dict, "__reduce__", no_pickle);
        class_method(&mut dict, "__reduce_ex__", no_pickle);
        class_method(&mut dict, "__getstate__", no_pickle);
        dict.insert(
            DictKey(Object::from_static("CONTINUE")),
            Object::Int(MODE_CONTINUE),
        );
        dict.insert(
            DictKey(Object::from_static("FLUSH_BLOCK")),
            Object::Int(MODE_FLUSH_BLOCK),
        );
        dict.insert(
            DictKey(Object::from_static("FLUSH_FRAME")),
            Object::Int(MODE_FLUSH_FRAME),
        );
        TypeObject::new_with_flags(
            "ZstdCompressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("ZstdCompressor must linearise")
    })
    .clone()
}

// ---------------------------------------------------------------------------
// ZstdDecompressor
// ---------------------------------------------------------------------------

fn decompressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let zdict = args
        .get(1)
        .cloned()
        .or_else(|| kwarg(kwargs, "zstd_dict").cloned());
    let options = args
        .get(2)
        .cloned()
        .or_else(|| kwarg(kwargs, "options").cloned());

    let dctx = unsafe { zstd_sys::ZSTD_createDCtx() };
    if dctx.is_null() {
        return Err(zstd_error("unable to create a ZSTD_DCtx"));
    }
    if let Some(opts) = &options {
        if !matches!(opts, Object::None) {
            apply_doptions(dctx, opts)?;
        }
    }
    if let Some(zd) = &zdict {
        if let Some((content, is_prefix)) = dict_arg(zd)? {
            if is_prefix {
                check_zstd(
                    unsafe {
                        zstd_sys::ZSTD_DCtx_refPrefix(
                            dctx,
                            content.as_ptr().cast::<c_void>(),
                            content.len(),
                        )
                    },
                    "unable to reference prefix",
                )?;
                inst.dict.borrow_mut().insert(
                    DictKey(Object::from_static("_prefix_keepalive")),
                    Object::new_bytes(content),
                );
            } else {
                check_zstd(
                    unsafe {
                        zstd_sys::ZSTD_DCtx_loadDictionary(
                            dctx,
                            content.as_ptr().cast::<c_void>(),
                            content.len(),
                        )
                    },
                    "unable to load dictionary",
                )?;
            }
        }
    }
    let id = next_id();
    if let Ok(mut reg) = decomp_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(DState {
                dctx,
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
    let max_length = match args.get(2).or_else(|| kwarg(kwargs, "max_length")) {
        Some(o) => crate::builtins::coerce_index_i64(o)?,
        None => -1,
    };
    let state = decomp_reg()
        .lock()
        .ok()
        .and_then(|reg| reg.get(&id).cloned())
        .ok_or_else(|| value_error("stale ZstdDecompressor"))?;
    let mut st = state.borrow_mut();
    if st.eof {
        return Err(eof_error("Already at the end of a Zstandard frame."));
    }
    let mut combined = std::mem::take(&mut st.input);
    combined.extend_from_slice(&data);
    let limit = if max_length < 0 {
        None
    } else {
        Some(max_length as usize)
    };
    let (out, consumed, frame_end) = decompress_step(st.dctx, &combined, limit)?;
    let leftover = combined[consumed..].to_vec();
    if frame_end {
        st.eof = true;
        st.needs_input = false;
        st.unused_data = leftover;
        st.input = Vec::new();
    } else if !leftover.is_empty() {
        // Output-capped with input still buffered: no new input needed.
        st.needs_input = false;
        st.input = leftover;
    } else {
        st.needs_input = true;
        st.input = Vec::new();
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

fn decompressor_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        class_method_kw(&mut dict, "__init__", decompressor_init);
        class_method_kw(&mut dict, "decompress", decompressor_decompress);
        class_method(&mut dict, "__reduce__", no_pickle);
        class_method(&mut dict, "__reduce_ex__", no_pickle);
        class_method(&mut dict, "__getstate__", no_pickle);
        TypeObject::new_with_flags(
            "ZstdDecompressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("ZstdDecompressor must linearise")
    })
    .clone()
}

// ---------------------------------------------------------------------------
// ZstdDict
// ---------------------------------------------------------------------------

fn zstddict_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let content = bytes_arg(args.get(1))?;
    let is_raw = match kwarg(kwargs, "is_raw") {
        Some(o) => o.is_truthy(),
        None => false,
    };
    if content.len() < 8 {
        return Err(value_error("Zstandard dictionary content too short"));
    }
    let dict_id =
        unsafe { zstd_sys::ZDICT_getDictID(content.as_ptr().cast::<c_void>(), content.len()) };
    if !is_raw && dict_id == 0 {
        return Err(value_error(
            "dict_content argument is not a valid Zstandard dictionary. The first 4 bytes of a valid Zstandard dictionary should be a magic number: b'\\x37\\xa4\\x30\\xec'.\nIf you are an advanced user, and can be sure that dict_content argument is a \"raw content\" dictionary, set is_raw parameter to True.",
        ));
    }
    let mut d = inst.dict.borrow_mut();
    d.insert(
        DictKey(Object::from_static("dict_content")),
        Object::new_bytes(content),
    );
    d.insert(
        DictKey(Object::from_static("dict_id")),
        Object::Int(i64::from(dict_id)),
    );
    Ok(Object::None)
}

fn zstddict_as_mode(mode: i64) -> impl Fn(&[Object]) -> Result<Object, RuntimeError> {
    move |args: &[Object]| {
        let inst = self_instance(args)?;
        Ok(Object::new_tuple(vec![
            Object::Instance(inst),
            Object::Int(mode),
        ]))
    }
}

fn zstddict_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let n = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("dict_content")))
        .and_then(|o| o.as_bytes_view())
        .map_or(0, |b| b.len());
    Ok(Object::Int(n as i64))
}

fn zstddict_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        class_method_kw(&mut dict, "__init__", zstddict_init);
        class_method(&mut dict, "__len__", zstddict_len);
        // The (dict, mode) advice tuples: 1 = digested (default), 2 =
        // undigested, 3 = prefix. Exposed as properties in CPython;
        // methods returning the same tuples cover the documented use
        // (passing them as `zstd_dict=`).
        for (name, mode) in [
            ("as_digested_dict", 1i64),
            ("as_undigested_dict", 2),
            ("as_prefix", 3),
        ] {
            let getter = Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(zstddict_as_mode(mode)),
                call_kw: None,
            }));
            dict.insert(
                DictKey(Object::from_static(name)),
                Object::Property(Rc::new(crate::object::PyProperty::new(
                    getter,
                    Object::None,
                    Object::None,
                    Object::None,
                ))),
            );
        }
        TypeObject::new_with_flags(
            "ZstdDict",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("ZstdDict must linearise")
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Module-level functions
// ---------------------------------------------------------------------------

/// `train_dict(chunks, chunk_sizes, dict_size)` — ZDICT_trainFromBuffer
/// over the concatenated samples.
fn m_train_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    let chunks = bytes_arg(args.first())?;
    let sizes = sample_sizes(args.get(1))?;
    let dict_size = args
        .get(2)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("dict_size must be an int object"))?;
    if dict_size <= 0 {
        return Err(value_error("dict_size argument should be positive number."));
    }
    let mut buf = vec![0u8; dict_size as usize];
    let n = unsafe {
        zstd_sys::ZDICT_trainFromBuffer(
            buf.as_mut_ptr().cast::<c_void>(),
            buf.len(),
            chunks.as_ptr().cast::<c_void>(),
            sizes.as_ptr(),
            sizes.len() as c_uint,
        )
    };
    if unsafe { zstd_sys::ZDICT_isError(n) } != 0 {
        let name = unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZDICT_getErrorName(n)) }
            .to_string_lossy()
            .into_owned();
        return Err(zstd_error(format!("cannot train dict: {name}")));
    }
    buf.truncate(n);
    Ok(Object::new_bytes(buf))
}

/// `finalize_dict(custom_dict_bytes, chunks, chunk_sizes, dict_size,
/// level)` — ZDICT_finalizeDictionary.
fn m_finalize_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    let base = bytes_arg(args.first())?;
    let chunks = bytes_arg(args.get(1))?;
    let sizes = sample_sizes(args.get(2))?;
    let dict_size = args
        .get(3)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("dict_size must be an int object"))?;
    let level = args
        .get(4)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("level must be an int object"))?;
    if dict_size <= 0 {
        return Err(value_error("dict_size argument should be positive number."));
    }
    let params = zstd_sys::ZDICT_params_t {
        compressionLevel: level as c_int,
        notificationLevel: 0,
        dictID: 0,
    };
    let mut buf = vec![0u8; dict_size as usize];
    let n = unsafe {
        zstd_sys::ZDICT_finalizeDictionary(
            buf.as_mut_ptr().cast::<c_void>(),
            buf.len(),
            base.as_ptr().cast::<c_void>(),
            base.len(),
            chunks.as_ptr().cast::<c_void>(),
            sizes.as_ptr(),
            sizes.len() as c_uint,
            params,
        )
    };
    if unsafe { zstd_sys::ZDICT_isError(n) } != 0 {
        let name = unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZDICT_getErrorName(n)) }
            .to_string_lossy()
            .into_owned();
        return Err(zstd_error(format!("cannot finalize dict: {name}")));
    }
    buf.truncate(n);
    Ok(Object::new_bytes(buf))
}

fn sample_sizes(arg: Option<&Object>) -> Result<Vec<usize>, RuntimeError> {
    let items = match arg {
        Some(Object::Tuple(items)) => items.to_vec(),
        Some(Object::List(items)) => items.borrow().clone(),
        _ => return Err(type_error("chunk_sizes must be a tuple")),
    };
    items
        .iter()
        .map(|o| {
            crate::builtins::coerce_index_i64(o).and_then(|v| {
                usize::try_from(v).map_err(|_| value_error("sample size out of range"))
            })
        })
        .collect()
}

/// `get_frame_info(frame_buffer)` → `(decompressed_size | None, dict_id)`.
fn m_get_frame_info(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_arg(args.first())?;
    let size =
        unsafe { zstd_sys::ZSTD_getFrameContentSize(data.as_ptr().cast::<c_void>(), data.len()) };
    // The sentinels are unsigned: CONTENTSIZE_UNKNOWN == 0ULL-1,
    // CONTENTSIZE_ERROR == 0ULL-2.
    const CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
    const CONTENTSIZE_ERROR: u64 = u64::MAX - 1;
    let size_obj = match size {
        CONTENTSIZE_UNKNOWN => Object::None,
        CONTENTSIZE_ERROR => {
            return Err(zstd_error(
                "Error when getting information from the header of a Zstandard frame. Ensure the frame_buffer argument starts from the beginning of a frame, and its length is not less than the frame header (6~18 bytes).",
            ))
        }
        n => Object::Int(n as i64),
    };
    let dict_id =
        unsafe { zstd_sys::ZSTD_getDictID_fromFrame(data.as_ptr().cast::<c_void>(), data.len()) };
    Ok(Object::new_tuple(vec![
        size_obj,
        Object::Int(i64::from(dict_id)),
    ]))
}

/// `get_frame_size(frame_buffer)` — the compressed size of the first
/// complete frame.
fn m_get_frame_size(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_arg(args.first())?;
    let n = unsafe {
        zstd_sys::ZSTD_findFrameCompressedSize(data.as_ptr().cast::<c_void>(), data.len())
    };
    check_zstd(
        n,
        "Error when finding the compressed size of a Zstandard frame",
    )?;
    Ok(Object::Int(n as i64))
}

/// `get_param_bounds(parameter, is_compress=True)` → `(lower, upper)`.
fn m_get_param_bounds(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let param = match args.first() {
        Some(o) => crate::builtins::coerce_index_i64(o)? as c_int,
        None => return Err(type_error("get_param_bounds requires a parameter")),
    };
    let is_compress = match args.get(1).or_else(|| kwarg(kwargs, "is_compress")) {
        Some(o) => o.is_truthy(),
        None => true,
    };
    let bounds = if is_compress {
        unsafe { zstd_sys::ZSTD_cParam_getBounds(cparam_from(i64::from(param))?) }
    } else {
        unsafe { zstd_sys::ZSTD_dParam_getBounds(dparam_from(i64::from(param))?) }
    };
    check_zstd(bounds.error, "invalid parameter")?;
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(bounds.lowerBound)),
        Object::Int(i64::from(bounds.upperBound)),
    ]))
}

/// `set_parameter_types(c, d)` — CPython registers the IntEnum types
/// for richer error messages; the twin accepts and records nothing.
fn m_set_parameter_types(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

fn register_kw(
    d: &mut DictData,
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) {
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    );
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_zstd"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Zstandard bindings (PEP 784; RFC 0076 WS15)."),
        );
        d.insert(
            DictKey(Object::from_static("ZstdCompressor")),
            Object::Type(compressor_class()),
        );
        d.insert(
            DictKey(Object::from_static("ZstdDecompressor")),
            Object::Type(decompressor_class()),
        );
        d.insert(
            DictKey(Object::from_static("ZstdDict")),
            Object::Type(zstddict_class()),
        );
        d.insert(
            DictKey(Object::from_static("ZstdError")),
            Object::Type(zstd_error_class()),
        );
        register(&mut d, "train_dict", m_train_dict);
        register(&mut d, "finalize_dict", m_finalize_dict);
        register(&mut d, "get_frame_info", m_get_frame_info);
        register(&mut d, "get_frame_size", m_get_frame_size);
        register_kw(&mut d, "get_param_bounds", m_get_param_bounds);
        register(&mut d, "set_parameter_types", m_set_parameter_types);

        let version_number = unsafe { zstd_sys::ZSTD_versionNumber() };
        let version = unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZSTD_versionString()) }
            .to_string_lossy()
            .into_owned();
        d.insert(
            DictKey(Object::from_static("zstd_version")),
            Object::from_str(version),
        );
        d.insert(
            DictKey(Object::from_static("zstd_version_number")),
            Object::Int(i64::from(version_number)),
        );
        d.insert(
            DictKey(Object::from_static("ZSTD_CLEVEL_DEFAULT")),
            Object::Int(i64::from(unsafe { zstd_sys::ZSTD_defaultCLevel() })),
        );
        d.insert(
            DictKey(Object::from_static("ZSTD_DStreamOutSize")),
            Object::Int(unsafe { zstd_sys::ZSTD_DStreamOutSize() } as i64),
        );

        // The compression-parameter codes, straight from the libzstd
        // enums (the Python layer builds its IntEnums from these).
        use zstd_sys::ZSTD_cParameter as C;
        use zstd_sys::ZSTD_dParameter as D;
        use zstd_sys::ZSTD_strategy as S;
        for (name, value) in [
            ("ZSTD_c_compressionLevel", C::ZSTD_c_compressionLevel as i64),
            ("ZSTD_c_windowLog", C::ZSTD_c_windowLog as i64),
            ("ZSTD_c_hashLog", C::ZSTD_c_hashLog as i64),
            ("ZSTD_c_chainLog", C::ZSTD_c_chainLog as i64),
            ("ZSTD_c_searchLog", C::ZSTD_c_searchLog as i64),
            ("ZSTD_c_minMatch", C::ZSTD_c_minMatch as i64),
            ("ZSTD_c_targetLength", C::ZSTD_c_targetLength as i64),
            ("ZSTD_c_strategy", C::ZSTD_c_strategy as i64),
            (
                "ZSTD_c_enableLongDistanceMatching",
                C::ZSTD_c_enableLongDistanceMatching as i64,
            ),
            ("ZSTD_c_ldmHashLog", C::ZSTD_c_ldmHashLog as i64),
            ("ZSTD_c_ldmMinMatch", C::ZSTD_c_ldmMinMatch as i64),
            ("ZSTD_c_ldmBucketSizeLog", C::ZSTD_c_ldmBucketSizeLog as i64),
            ("ZSTD_c_ldmHashRateLog", C::ZSTD_c_ldmHashRateLog as i64),
            ("ZSTD_c_contentSizeFlag", C::ZSTD_c_contentSizeFlag as i64),
            ("ZSTD_c_checksumFlag", C::ZSTD_c_checksumFlag as i64),
            ("ZSTD_c_dictIDFlag", C::ZSTD_c_dictIDFlag as i64),
            ("ZSTD_c_nbWorkers", C::ZSTD_c_nbWorkers as i64),
            ("ZSTD_c_jobSize", C::ZSTD_c_jobSize as i64),
            ("ZSTD_c_overlapLog", C::ZSTD_c_overlapLog as i64),
            ("ZSTD_d_windowLogMax", D::ZSTD_d_windowLogMax as i64),
            ("ZSTD_fast", S::ZSTD_fast as i64),
            ("ZSTD_dfast", S::ZSTD_dfast as i64),
            ("ZSTD_greedy", S::ZSTD_greedy as i64),
            ("ZSTD_lazy", S::ZSTD_lazy as i64),
            ("ZSTD_lazy2", S::ZSTD_lazy2 as i64),
            ("ZSTD_btlazy2", S::ZSTD_btlazy2 as i64),
            ("ZSTD_btopt", S::ZSTD_btopt as i64),
            ("ZSTD_btultra", S::ZSTD_btultra as i64),
            ("ZSTD_btultra2", S::ZSTD_btultra2 as i64),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(value));
        }
    }
    Rc::new(PyModule {
        name: "_zstd".to_owned(),
        filename: None,
        dict,
    })
}
