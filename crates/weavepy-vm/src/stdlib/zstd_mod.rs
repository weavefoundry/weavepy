//! `_zstd`: Zstandard bindings (PEP 784; RFC 0076 WS15).
//!
//! The native core under the verbatim CPython 3.14 `compression.zstd`
//! package (`python/compression/zstd/`). Backed by libzstd through
//! `zstd-sys` (vendored/static, matching the bzip2/lzma posture): the
//! streaming engine is `ZSTD_compressStream2` / `ZSTD_decompressStream`
//! driven exactly the way CPython's `Modules/_zstd` drives it, so the
//! `mode=`/`max_length=` semantics and the frame-boundary behaviour
//! (one `ZstdDecompressor` per frame, `unused_data` after `eof`) are
//! the documented ones. Dictionaries follow CPython's three loading
//! modes (digested `ZSTD_CDict`/`ZSTD_DDict`, undigested
//! `loadDictionary`, raw prefix), and the option dictionaries reject
//! keys from the wrong `*Parameter` IntEnum the way CPython's
//! `set_parameter_types` registration does.
//!
//! State lives in a process-global registry keyed by the integer
//! handle stored on each instance's `_handle`, the `_bz2`/`zlib`
//! streaming-object pattern.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{overflow_error, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

// ---------------------------------------------------------------------------
// ZstdError
// ---------------------------------------------------------------------------

/// `_zstd.ZstdError`: an `Exception` subclass, as CPython creates it.
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

/// `ZSTD_getErrorName` as an owned string.
fn zstd_error_name(code: usize) -> String {
    unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZSTD_getErrorName(code)) }
        .to_string_lossy()
        .into_owned()
}

/// Map a libzstd error code to `ZstdError("<what>: <name>")`, the shape
/// of CPython's `set_zstd_error` messages.
fn check_zstd(code: usize, what: &str) -> Result<usize, RuntimeError> {
    if unsafe { zstd_sys::ZSTD_isError(code) } != 0 {
        return Err(zstd_error(format!("{what}: {}", zstd_error_name(code))));
    }
    Ok(code)
}

// ---------------------------------------------------------------------------
// Native engine state
// ---------------------------------------------------------------------------

struct CState {
    cctx: *mut zstd_sys::ZSTD_CCtx,
    last_mode: i64,
    /// Digested dictionary referenced by `cctx` (`as_digested_dict`);
    /// freed after the context, as CPython frees the `ZSTD_CDict` after
    /// `ZSTD_freeCCtx`.
    cdict: *mut zstd_sys::ZSTD_CDict,
    /// Prefix bytes referenced by `cctx`. `ZSTD_CCtx_refPrefix` borrows
    /// the buffer rather than copying it, so the bytes must live exactly
    /// as long as the context (test_zstd test_as_prefix diffs a 100 KB
    /// prefix against the whole test file).
    prefix: Option<Vec<u8>>,
}

struct DState {
    dctx: *mut zstd_sys::ZSTD_DCtx,
    /// Unconsumed compressed input carried across calls (CPython's
    /// `input_buffer[in_begin..in_end]`).
    input: Vec<u8>,
    eof: bool,
    needs_input: bool,
    unused_data: Vec<u8>,
    /// Digested dictionary referenced by `dctx` (the decompressor's
    /// default loading mode); freed after the context.
    ddict: *mut zstd_sys::ZSTD_DDict,
    /// Prefix bytes referenced by `dctx` (see `CState::prefix`).
    prefix: Option<Vec<u8>>,
}

impl Drop for CState {
    fn drop(&mut self) {
        unsafe { zstd_sys::ZSTD_freeCCtx(self.cctx) };
        if !self.cdict.is_null() {
            unsafe { zstd_sys::ZSTD_freeCDict(self.cdict) };
        }
        // Only now may the borrowed prefix bytes go away.
        drop(self.prefix.take());
    }
}

impl Drop for DState {
    fn drop(&mut self) {
        unsafe { zstd_sys::ZSTD_freeDCtx(self.dctx) };
        if !self.ddict.is_null() {
            unsafe { zstd_sys::ZSTD_freeDDict(self.ddict) };
        }
        drop(self.prefix.take());
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

/// The `CompressionParameter` / `DecompressionParameter` IntEnum types
/// handed over by `set_parameter_types`. CPython keeps them in the
/// module state so the option-dict loops can reject a key of the wrong
/// enum with a TypeError (test_zstd test_init_bad_mode).
struct ParameterTypes {
    c_parameter: Option<Rc<TypeObject>>,
    d_parameter: Option<Rc<TypeObject>>,
}

fn parameter_types() -> &'static Mutex<ParameterTypes> {
    static TYPES: OnceLock<Mutex<ParameterTypes>> = OnceLock::new();
    TYPES.get_or_init(|| {
        Mutex::new(ParameterTypes {
            c_parameter: None,
            d_parameter: None,
        })
    })
}

/// `Py_TYPE(key) == <registered enum type>`: an exact-type check on an
/// IntEnum member (an instance carrying an int payload).
fn key_has_type(key: &Object, ty: Option<&Rc<TypeObject>>) -> bool {
    match (key, ty) {
        (Object::Instance(inst), Some(t)) => Rc::ptr_eq(&inst.cls(), t),
        _ => false,
    }
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

/// A PEP 688 exporter read through its `__buffer__` hook, the way
/// CPython's `Py_buffer` converter goes through `bf_getbuffer` for
/// objects that aren't bytes-like natively (mirrors `mmap`'s helper).
fn buffer_via_dunder(obj: &Object) -> Result<Option<Vec<u8>>, RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(None);
    };
    // SAFETY: the pointer was published by an enclosing VM frame still
    // live on this thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    match interp.memoryview_from_object_and_flags(obj, 0, &globals)? {
        Some(Object::MemoryView(mv)) => Ok(Some(mv.to_bytes())),
        _ => Ok(None),
    }
}

/// CPython `Py_buffer` argument: bytes, bytearray, memoryview, a bytes
/// subclass, or any other buffer exporter (test_zstd
/// OpenTestCase.test_buffer_protocol writes an `array.array` through
/// `ZstdFile.write`, which lands in `ZstdCompressor.compress`).
fn buffer_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    let Some(obj) = arg else {
        return Err(type_error("a bytes-like object is required"));
    };
    if let Some(v) = obj.as_bytes_view() {
        return Ok(v);
    }
    if let Object::Instance(inst) = obj {
        if let Some(v) = inst.native.get().and_then(Object::as_bytes_view) {
            return Ok(v);
        }
        if let Some(v) = buffer_via_dunder(obj)? {
            return Ok(v);
        }
    }
    Err(type_error(format!(
        "a bytes-like object is required, not '{}'",
        obj.type_name_owned()
    )))
}

/// Argument Clinic `PyBytesObject`: `bytes` (or a subclass) only. A
/// bytearray is a TypeError here (test_zstd test_train_dict_c /
/// test_finalize_dict_c), unlike the `Py_buffer` arguments elsewhere.
fn exact_bytes_arg(arg: Option<&Object>, func: &str, pos: usize) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::Instance(inst)) if matches!(inst.native.get(), Some(Object::Bytes(_))) => {
            Ok(inst
                .native
                .get()
                .and_then(Object::as_bytes_view)
                .unwrap_or_default())
        }
        Some(other) => Err(type_error(format!(
            "{func}() argument {pos} must be bytes, not {}",
            other.type_name_owned()
        ))),
        None => Err(type_error(format!(
            "{func}() missing required argument {pos}"
        ))),
    }
}

/// `PyLong_Check`: a real int (bool, small or big) or an int-subclass
/// instance carrying an int payload (IntEnum members).
fn is_int_object(o: &Object) -> bool {
    match o {
        Object::Int(_) | Object::Bool(_) | Object::Long(_) => true,
        Object::Instance(inst) => matches!(
            inst.native.get(),
            Some(Object::Int(_) | Object::Bool(_) | Object::Long(_))
        ),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Streaming engine
// ---------------------------------------------------------------------------

/// One `compress(data, mode)` / `flush(mode)` call: drive
/// `ZSTD_compressStream2` until it reports nothing left to flush
/// (CPython `compress_lock_held` loops until the return value is 0,
/// growing the output buffer whenever it fills).
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
    let continuing = matches!(end_op, zstd_sys::ZSTD_EndDirective::ZSTD_e_continue);
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
            "Unable to compress Zstandard data",
        )?;
        out.extend_from_slice(&buf[..out_buf.pos]);
        if remaining == 0 {
            break;
        }
        // CPython `compress_mt_continue_lock_held`: in CONTINUE mode the
        // call is done once the input is consumed and the output buffer
        // still has room (multi-threaded contexts report pending work
        // that only a later call can collect).
        if continuing && in_buf.pos == in_buf.size && out_buf.pos < out_buf.size {
            break;
        }
    }
    Ok(out)
}

/// One `decompress(data, max_length)` call over a single frame, the loop
/// of CPython's `decompress_lock_held`. Returns `(output,
/// input_consumed, frame_complete)`.
///
/// `ZSTD_decompressStream` is always called at least once, even with
/// `max_length == 0`: a skippable frame is consumed without producing
/// output, so `decompress(SKIPPABLE_FRAME, 0)` must still reach `eof`
/// (test_zstd DecompressorFlagsTestCase.test_decompressor_skippable).
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
            Some(l) => l.saturating_sub(out.len()).min(buf.len()),
            None => buf.len(),
        };
        let mut out_buf = zstd_sys::ZSTD_outBuffer {
            dst: buf.as_mut_ptr().cast::<c_void>(),
            size: room,
            pos: 0,
        };
        let ret = check_zstd(
            unsafe { zstd_sys::ZSTD_decompressStream(dctx, &raw mut out_buf, &raw mut in_buf) },
            "Unable to decompress Zstandard data",
        )?;
        out.extend_from_slice(&buf[..out_buf.pos]);
        if ret == 0 {
            // Frame complete; anything left in `in_buf` is trailing data.
            frame_end = true;
            break;
        }
        // Check the output before the input: zstd's internal buffer may
        // still hold bytes that a bigger output buffer would receive.
        if out_buf.pos == out_buf.size {
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
            // Grow (the next iteration hands zstd a fresh buffer).
        } else if in_buf.pos == in_buf.size {
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
/// libzstd enum (never transmuted: an invalid discriminant would be UB).
/// Unknown codes yield `None`; the caller reports them the way CPython's
/// `set_parameter_error` does.
fn cparam_from(code: i64) -> Option<(zstd_sys::ZSTD_cParameter, &'static str)> {
    use zstd_sys::ZSTD_cParameter as C;
    Some(match code {
        100 => (C::ZSTD_c_compressionLevel, "compression_level"),
        101 => (C::ZSTD_c_windowLog, "window_log"),
        102 => (C::ZSTD_c_hashLog, "hash_log"),
        103 => (C::ZSTD_c_chainLog, "chain_log"),
        104 => (C::ZSTD_c_searchLog, "search_log"),
        105 => (C::ZSTD_c_minMatch, "min_match"),
        106 => (C::ZSTD_c_targetLength, "target_length"),
        107 => (C::ZSTD_c_strategy, "strategy"),
        160 => (
            C::ZSTD_c_enableLongDistanceMatching,
            "enable_long_distance_matching",
        ),
        161 => (C::ZSTD_c_ldmHashLog, "ldm_hash_log"),
        162 => (C::ZSTD_c_ldmMinMatch, "ldm_min_match"),
        163 => (C::ZSTD_c_ldmBucketSizeLog, "ldm_bucket_size_log"),
        164 => (C::ZSTD_c_ldmHashRateLog, "ldm_hash_rate_log"),
        200 => (C::ZSTD_c_contentSizeFlag, "content_size_flag"),
        201 => (C::ZSTD_c_checksumFlag, "checksum_flag"),
        202 => (C::ZSTD_c_dictIDFlag, "dict_id_flag"),
        400 => (C::ZSTD_c_nbWorkers, "nb_workers"),
        401 => (C::ZSTD_c_jobSize, "job_size"),
        402 => (C::ZSTD_c_overlapLog, "overlap_log"),
        _ => return None,
    })
}

fn dparam_from(code: i64) -> Option<(zstd_sys::ZSTD_dParameter, &'static str)> {
    use zstd_sys::ZSTD_dParameter as D;
    Some(match code {
        100 => (D::ZSTD_d_windowLogMax, "window_log_max"),
        _ => return None,
    })
}

/// CPython 3.14 `PyLong_AsInt` on an option key/value: `__index__`, then
/// a C `int` range check.
fn option_c_int(o: &Object) -> Result<c_int, RuntimeError> {
    let v = crate::builtins::coerce_index_i64(o)?;
    c_int::try_from(v).map_err(|_| overflow_error("Python int too large to convert to C int"))
}

/// CPython 3.14 `set_parameter_error`: a rejected option is a ValueError
/// naming the parameter and its libzstd bounds; an unknown key is
/// "invalid compression parameter 'unknown parameter (key N)'"
/// (test_zstd test_compress_parameters / test_unknown_compression_parameter).
fn parameter_error(
    kind: &str,
    name: Option<&str>,
    code: i64,
    value: c_int,
    bounds: Option<zstd_sys::ZSTD_bounds>,
) -> RuntimeError {
    let name = name.map_or_else(|| format!("unknown parameter (key {code})"), str::to_owned);
    match bounds {
        Some(b) if unsafe { zstd_sys::ZSTD_isError(b.error) } == 0 => value_error(format!(
            "{kind} parameter '{name}' received an illegal value {value}; \
             the valid range is [{}, {}]",
            b.lowerBound, b.upperBound
        )),
        _ => value_error(format!("invalid {kind} parameter '{name}'")),
    }
}

/// CPython 3.14 `_zstd_set_c_level`: range-check against
/// `ZSTD_minCLevel()..=ZSTD_maxCLevel()` before touching the context.
fn set_c_level(cctx: *mut zstd_sys::ZSTD_CCtx, level: c_int) -> Result<(), RuntimeError> {
    let (min, max) = unsafe { (zstd_sys::ZSTD_minCLevel(), zstd_sys::ZSTD_maxCLevel()) };
    if level < min || level > max {
        return Err(value_error(format!(
            "illegal compression level {level}; the valid range is [{min}, {max}]"
        )));
    }
    check_zstd(
        unsafe {
            zstd_sys::ZSTD_CCtx_setParameter(
                cctx,
                zstd_sys::ZSTD_cParameter::ZSTD_c_compressionLevel,
                level,
            )
        },
        "Unable to set zstd compression level",
    )?;
    Ok(())
}

/// Apply an `options` dict of `{parameter_code: value}` (keys/values
/// coerced via `__index__`, so `CompressionParameter` IntEnum members
/// work directly). A `compression_level` entry also updates `level`,
/// which CPython keeps for building a digested `ZSTD_CDict`.
fn apply_coptions(
    cctx: *mut zstd_sys::ZSTD_CCtx,
    options: &Object,
    level: &mut c_int,
) -> Result<(), RuntimeError> {
    let Object::Dict(d) = options else {
        return Err(type_error(format!(
            "ZstdCompressor() argument 'options' must be dict, not {}",
            options.type_name_owned()
        )));
    };
    let items: Vec<(Object, Object)> = d
        .borrow()
        .iter()
        .map(|(k, v)| (k.0.clone(), v.clone()))
        .collect();
    let d_parameter = parameter_types()
        .lock()
        .ok()
        .and_then(|t| t.d_parameter.clone());
    for (k, v) in items {
        // CPython `_zstd_set_c_parameters`: a DecompressionParameter key
        // is rejected before any int coercion (test_zstd
        // test_init_bad_mode).
        if key_has_type(&k, d_parameter.as_ref()) {
            return Err(type_error(
                "compression options dictionary key must not be a DecompressionParameter attribute",
            ));
        }
        let code = i64::from(option_c_int(&k)?);
        let value = option_c_int(&v)?;
        let Some((param, name)) = cparam_from(code) else {
            return Err(parameter_error("compression", None, code, value, None));
        };
        if code == 100 {
            set_c_level(cctx, value)?;
            *level = value;
            continue;
        }
        let ret = unsafe { zstd_sys::ZSTD_CCtx_setParameter(cctx, param, value) };
        if unsafe { zstd_sys::ZSTD_isError(ret) } != 0 {
            let bounds = unsafe { zstd_sys::ZSTD_cParam_getBounds(param) };
            return Err(parameter_error(
                "compression",
                Some(name),
                code,
                value,
                Some(bounds),
            ));
        }
    }
    Ok(())
}

fn apply_doptions(dctx: *mut zstd_sys::ZSTD_DCtx, options: &Object) -> Result<(), RuntimeError> {
    let Object::Dict(d) = options else {
        return Err(type_error(format!(
            "ZstdDecompressor() argument 'options' must be dict, not {}",
            options.type_name_owned()
        )));
    };
    let items: Vec<(Object, Object)> = d
        .borrow()
        .iter()
        .map(|(k, v)| (k.0.clone(), v.clone()))
        .collect();
    let c_parameter = parameter_types()
        .lock()
        .ok()
        .and_then(|t| t.c_parameter.clone());
    for (k, v) in items {
        // CPython `_zstd_set_d_parameters`: a CompressionParameter key is
        // a TypeError, not a bounds ValueError on `window_log_max`
        // (test_zstd test_init_bad_mode opens a ZstdFile for reading
        // with `{CompressionParameter.compression_level: 5}`).
        if key_has_type(&k, c_parameter.as_ref()) {
            return Err(type_error(
                "decompression options dictionary key must not be a CompressionParameter attribute",
            ));
        }
        let code = i64::from(option_c_int(&k)?);
        let value = option_c_int(&v)?;
        let Some((param, name)) = dparam_from(code) else {
            return Err(parameter_error("decompression", None, code, value, None));
        };
        let ret = unsafe { zstd_sys::ZSTD_DCtx_setParameter(dctx, param, value) };
        if unsafe { zstd_sys::ZSTD_isError(ret) } != 0 {
            let bounds = unsafe { zstd_sys::ZSTD_dParam_getBounds(param) };
            return Err(parameter_error(
                "decompression",
                Some(name),
                code,
                value,
                Some(bounds),
            ));
        }
    }
    Ok(())
}

/// CPython's `dictionary_type` enum: the second item of the
/// `(ZstdDict, type)` tuples returned by `as_digested_dict`,
/// `as_undigested_dict`, and `as_prefix`. Any other value makes the tuple
/// "not a ZstdDict object" (test_zstd test_invalid_dict rejects
/// `(zd, -1)` and `(zd, 3)`).
const DICT_TYPE_DIGESTED: i64 = 0;
const DICT_TYPE_UNDIGESTED: i64 = 1;
const DICT_TYPE_PREFIX: i64 = 2;

/// `PyObject_TypeCheck(obj, ZstdDict_type)`.
fn zstddict_instance(obj: &Object) -> Option<Rc<PyInstance>> {
    match obj {
        Object::Instance(inst) if inst.cls().is_subclass_of(&zstddict_class()) => {
            Some(inst.clone())
        }
        _ => None,
    }
}

/// The dictionary bytes stored by `ZstdDict.__init__`.
fn zstddict_content(inst: &PyInstance) -> Result<Vec<u8>, RuntimeError> {
    inst.dict
        .borrow()
        .get(&DictKey(Object::from_static("_dict_content")))
        .and_then(Object::as_bytes_view)
        .ok_or_else(|| type_error("zstd_dict argument should be a ZstdDict object."))
}

/// CPython `_Py_parse_zstd_dict`: resolve a `zstd_dict=` argument, a
/// `ZstdDict` instance (loaded as `default_type`) or one of the
/// `(ZstdDict, type)` advice tuples. Yields `(dict_content, type)`.
fn parse_zstd_dict(obj: &Object, default_type: i64) -> Result<(Vec<u8>, i64), RuntimeError> {
    if let Some(inst) = zstddict_instance(obj) {
        return Ok((zstddict_content(&inst)?, default_type));
    }
    if let Object::Tuple(items) = obj {
        if items.len() == 2 {
            if let Some(inst) = zstddict_instance(&items[0]) {
                // `PyLong_Check` then `PyLong_AsInt`: a float second item
                // is the generic TypeError below, a huge int is an
                // OverflowError (test_invalid_dict).
                if is_int_object(&items[1]) {
                    let kind = i64::from(option_c_int(&items[1])?);
                    if (DICT_TYPE_DIGESTED..=DICT_TYPE_PREFIX).contains(&kind) {
                        return Ok((zstddict_content(&inst)?, kind));
                    }
                }
            }
        }
    }
    Err(type_error(
        "zstd_dict argument should be a ZstdDict object.",
    ))
}

/// CPython `_zstd_load_c_dict`: compressors load an undigested
/// dictionary by default; `as_digested_dict` builds a `ZSTD_CDict` at
/// the compressor's level up front, so a corrupt dictionary fails here
/// with "Failed to create a ZSTD_CDict instance from Zstandard
/// dictionary content." (test_zstd test_invalid_dict).
fn load_c_dict(state: &mut CState, obj: &Object, level: c_int) -> Result<(), RuntimeError> {
    let (content, kind) = parse_zstd_dict(obj, DICT_TYPE_UNDIGESTED)?;
    let ret = match kind {
        DICT_TYPE_DIGESTED => {
            let cdict = unsafe {
                zstd_sys::ZSTD_createCDict(content.as_ptr().cast::<c_void>(), content.len(), level)
            };
            if cdict.is_null() {
                return Err(zstd_error(
                    "Failed to create a ZSTD_CDict instance from Zstandard dictionary content.",
                ));
            }
            // Owned by the state so it outlives the context's reference.
            state.cdict = cdict;
            unsafe { zstd_sys::ZSTD_CCtx_refCDict(state.cctx, cdict) }
        }
        DICT_TYPE_UNDIGESTED => unsafe {
            zstd_sys::ZSTD_CCtx_loadDictionary(
                state.cctx,
                content.as_ptr().cast::<c_void>(),
                content.len(),
            )
        },
        _ => {
            let ret = unsafe {
                zstd_sys::ZSTD_CCtx_refPrefix(
                    state.cctx,
                    content.as_ptr().cast::<c_void>(),
                    content.len(),
                )
            };
            // The prefix is borrowed, not copied: park the bytes in the
            // state (moving a Vec keeps its heap buffer in place).
            state.prefix = Some(content);
            ret
        }
    };
    check_zstd(
        ret,
        "Unable to load Zstandard dictionary or prefix for compression",
    )?;
    Ok(())
}

/// CPython `_zstd_load_d_dict`: decompressors load a digested
/// `ZSTD_DDict` by default (a corrupt dictionary fails with "Failed to
/// create a ZSTD_DDict instance from Zstandard dictionary content.").
fn load_d_dict(state: &mut DState, obj: &Object) -> Result<(), RuntimeError> {
    let (content, kind) = parse_zstd_dict(obj, DICT_TYPE_DIGESTED)?;
    let ret = match kind {
        DICT_TYPE_DIGESTED => {
            let ddict = unsafe {
                zstd_sys::ZSTD_createDDict(content.as_ptr().cast::<c_void>(), content.len())
            };
            if ddict.is_null() {
                return Err(zstd_error(
                    "Failed to create a ZSTD_DDict instance from Zstandard dictionary content.",
                ));
            }
            state.ddict = ddict;
            unsafe { zstd_sys::ZSTD_DCtx_refDDict(state.dctx, ddict) }
        }
        DICT_TYPE_UNDIGESTED => unsafe {
            zstd_sys::ZSTD_DCtx_loadDictionary(
                state.dctx,
                content.as_ptr().cast::<c_void>(),
                content.len(),
            )
        },
        _ => {
            let ret = unsafe {
                zstd_sys::ZSTD_DCtx_refPrefix(
                    state.dctx,
                    content.as_ptr().cast::<c_void>(),
                    content.len(),
                )
            };
            state.prefix = Some(content);
            ret
        }
    };
    check_zstd(
        ret,
        "Unable to load Zstandard dictionary or prefix for decompression",
    )?;
    Ok(())
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
        .or_else(|| kwarg(kwargs, "level").cloned())
        .unwrap_or(Object::None);
    let options = args
        .get(2)
        .cloned()
        .or_else(|| kwarg(kwargs, "options").cloned())
        .unwrap_or(Object::None);
    let zdict = args
        .get(3)
        .cloned()
        .or_else(|| kwarg(kwargs, "zstd_dict").cloned())
        .unwrap_or(Object::None);

    let cctx = unsafe { zstd_sys::ZSTD_createCCtx() };
    if cctx.is_null() {
        return Err(zstd_error("Unable to create ZSTD_CCtx instance."));
    }
    // `state` owns the context from here on: any early return below
    // drops it and frees the context (and a digested dictionary).
    let mut state = CState {
        cctx,
        last_mode: MODE_FLUSH_FRAME,
        cdict: std::ptr::null_mut(),
        prefix: None,
    };
    // CPython `ZstdCompressor.__new__` keeps the effective level (default
    // ZSTD_CLEVEL_DEFAULT) for `_get_CDict`.
    let mut compression_level: c_int = unsafe { zstd_sys::ZSTD_defaultCLevel() };

    if !matches!(level, Object::None) && !matches!(options, Object::None) {
        return Err(type_error("Only one of level or options should be used."));
    }
    if !matches!(level, Object::None) {
        // CPython 3.14 `ZstdCompressor.__new__`: `level` must be an int;
        // one outside C `int` reports the level range without the value,
        // and one inside it goes through `_zstd_set_c_level` (test_zstd
        // test_compress_parameters).
        if !is_int_object(&level) {
            return Err(type_error("invalid type for level, expected int"));
        }
        let level_v = match level.as_i64().and_then(|v| c_int::try_from(v).ok()) {
            Some(v) => v,
            None => {
                let (min, max) =
                    unsafe { (zstd_sys::ZSTD_minCLevel(), zstd_sys::ZSTD_maxCLevel()) };
                return Err(value_error(format!(
                    "illegal compression level; the valid range is [{min}, {max}]"
                )));
            }
        };
        set_c_level(cctx, level_v)?;
        compression_level = level_v;
    }
    if !matches!(options, Object::None) {
        apply_coptions(cctx, &options, &mut compression_level)?;
    }
    if !matches!(zdict, Object::None) {
        load_c_dict(&mut state, &zdict, compression_level)?;
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

/// Run one compression step and record `last_mode`. On error CPython
/// resets `last_mode` to FLUSH_FRAME and the context's session
/// (`ZSTD_CCtx_reset(ZSTD_reset_session_only)`), so the compressor can
/// start a fresh frame afterwards.
fn run_compress(
    args: &[Object],
    st: &mut CState,
    input: &[u8],
    mode: i64,
) -> Result<Object, RuntimeError> {
    let dir = end_directive(mode)?;
    match compress_step(st.cctx, input, dir) {
        Ok(out) => {
            note_last_mode(args, st, mode);
            Ok(Object::new_bytes(out))
        }
        Err(e) => {
            note_last_mode(args, st, MODE_FLUSH_FRAME);
            let _ = unsafe {
                zstd_sys::ZSTD_CCtx_reset(
                    st.cctx,
                    zstd_sys::ZSTD_ResetDirective::ZSTD_reset_session_only,
                )
            };
            Err(e)
        }
    }
}

fn compressor_compress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = buffer_arg(args.get(1))?;
    let mode = match args.get(2).or_else(|| kwarg(kwargs, "mode")) {
        Some(o) => crate::builtins::coerce_index_i64(o)?,
        None => MODE_CONTINUE,
    };
    // Validate the mode before touching the context.
    end_directive(mode)?;
    let state = comp_state(id)?;
    let mut st = state.borrow_mut();
    run_compress(args, &mut st, &data, mode)
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
    let state = comp_state(id)?;
    let mut st = state.borrow_mut();
    run_compress(args, &mut st, &[], mode)
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
    // CPython 3.14: `size` is `None` (ZSTD_CONTENTSIZE_UNKNOWN) or an int in
    // `[0, ZSTD_CONTENTSIZE_ERROR)`; the two sentinel values and anything
    // negative or past 2**64 share one ValueError (test_zstd
    // test_set_pledged_input_size).
    const CONTENTSIZE_ERROR: u64 = u64::MAX - 1;
    let size = match args.get(1) {
        None | Some(Object::None) => u64::MAX, // ZSTD_CONTENTSIZE_UNKNOWN
        Some(o) => {
            use num_traits::ToPrimitive;
            let too_big = || {
                value_error(format!(
                    "size argument should be a positive int less than {CONTENTSIZE_ERROR}"
                ))
            };
            let v: Option<u64> = match o {
                Object::Int(i) => u64::try_from(*i).ok(),
                Object::Bool(b) => Some(u64::from(*b)),
                Object::Long(b) => b.to_u64(),
                _ => return Err(type_error("an integer is required")),
            };
            match v {
                Some(v) if v < CONTENTSIZE_ERROR => v,
                _ => return Err(too_big()),
            }
        }
    };
    check_zstd(
        unsafe { zstd_sys::ZSTD_CCtx_setPledgedSrcSize(st.cctx, size) },
        "Unable to set pledged uncompressed content size",
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

/// A read-only data descriptor whose getter is a bound builtin (the
/// shape of CPython's `PyGetSetDef` entries: assignment raises
/// AttributeError, which test_zstd asserts for `dict_content`,
/// `dict_id`, and the `as_*` accessors).
fn class_getter(
    dict: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    let getter = Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
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

/// Publish the decompressor flags on the instance (`eof`, `needs_input`,
/// `unused_data`; CPython exposes them as read-only members/getters).
fn sync_decompressor_flags(args: &[Object], st: &DState) {
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
}

fn decompressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let zdict = args
        .get(1)
        .cloned()
        .or_else(|| kwarg(kwargs, "zstd_dict").cloned())
        .unwrap_or(Object::None);
    let options = args
        .get(2)
        .cloned()
        .or_else(|| kwarg(kwargs, "options").cloned())
        .unwrap_or(Object::None);

    let dctx = unsafe { zstd_sys::ZSTD_createDCtx() };
    if dctx.is_null() {
        return Err(zstd_error("Unable to create ZSTD_DCtx instance."));
    }
    let mut state = DState {
        dctx,
        input: Vec::new(),
        eof: false,
        needs_input: true,
        unused_data: Vec::new(),
        ddict: std::ptr::null_mut(),
        prefix: None,
    };
    // CPython `ZstdDecompressor.__new__` loads the dictionary first, then
    // applies the options.
    if !matches!(zdict, Object::None) {
        load_d_dict(&mut state, &zdict)?;
    }
    if !matches!(options, Object::None) {
        apply_doptions(dctx, &options)?;
    }
    sync_decompressor_flags(args, &state);
    let id = next_id();
    if let Ok(mut reg) = decomp_reg().lock() {
        reg.insert(id, Rc::new(RefCell::new(state)));
    }
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    Ok(Object::None)
}

/// CPython `stream_decompress_lock_held`: feed buffered-plus-new input
/// through one `decompress_lock_held` pass, then set the flags.
///
/// * `eof` flips when a frame completes; further calls raise EOFError.
/// * `needs_input` is False when input is left over, when the output
///   hit `max_length` exactly, or after `eof`; otherwise True
///   (test_zstd test_decompress_epilogue_flags, test_decompressor_1).
/// * `unused_data` is the leftover input once `eof` is set, else b''.
///
/// An error resets the session (`decompressor_reset_session_lock_held`).
fn decompressor_decompress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = buffer_arg(args.get(1))?;
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
    let (out, consumed, frame_end) = match decompress_step(st.dctx, &combined, limit) {
        Ok(step) => step,
        Err(e) => {
            st.input = Vec::new();
            st.unused_data = Vec::new();
            st.needs_input = true;
            st.eof = false;
            let _ = unsafe {
                zstd_sys::ZSTD_DCtx_reset(
                    st.dctx,
                    zstd_sys::ZSTD_ResetDirective::ZSTD_reset_session_only,
                )
            };
            sync_decompressor_flags(args, &st);
            return Err(e);
        }
    };
    if frame_end {
        st.eof = true;
    }
    if consumed == combined.len() {
        // All input consumed: more is wanted unless the output was capped
        // at exactly `max_length` (zstd may hold more) or the frame ended.
        st.needs_input = !(limit == Some(out.len()) || st.eof);
        st.input = Vec::new();
    } else {
        st.needs_input = false;
        st.input = combined[consumed..].to_vec();
    }
    st.unused_data = if st.eof { st.input.clone() } else { Vec::new() };
    sync_decompressor_flags(args, &st);
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

/// `ZstdDict(dict_content, /, *, is_raw=False)`, CPython's
/// `_zstd_ZstdDict_new_impl`: `dict_content` is positional-only and
/// `is_raw` keyword-only (`ZstdDict(bytes(8), True)` is a TypeError in
/// test_zstd test_is_raw), the content must be at least eight bytes, and
/// without `is_raw` it must carry a dictionary ID.
fn zstddict_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    if args.len() > 2 {
        return Err(type_error(format!(
            "ZstdDict() takes at most 1 positional argument ({} given)",
            args.len() - 1
        )));
    }
    for (k, _) in kwargs {
        if k != "is_raw" {
            return Err(type_error(format!(
                "ZstdDict() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    if args.len() < 2 {
        return Err(type_error(
            "ZstdDict() missing required argument 'dict_content' (pos 1)",
        ));
    }
    let content = buffer_arg(args.get(1))?;
    let is_raw = kwarg(kwargs, "is_raw").is_some_and(Object::is_truthy);
    if content.len() < 8 {
        return Err(value_error(
            "Zstandard dictionary content too short (must have at least eight bytes)",
        ));
    }
    // 0 means a "raw content" dictionary.
    let dict_id = unsafe {
        zstd_sys::ZSTD_getDictID_fromDict(content.as_ptr().cast::<c_void>(), content.len())
    };
    if !is_raw && dict_id == 0 {
        return Err(value_error("invalid Zstandard dictionary"));
    }
    let mut d = inst.dict.borrow_mut();
    d.insert(
        DictKey(Object::from_static("_dict_content")),
        Object::new_bytes(content),
    );
    d.insert(
        DictKey(Object::from_static("_dict_id")),
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

/// `ZstdDict.dict_content` getter: the bytes object.
fn zstddict_dict_content(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    Ok(Object::new_bytes(zstddict_content(&inst)?))
}

/// `ZstdDict.dict_id` getter.
fn zstddict_dict_id(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let id = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_dict_id")))
        .cloned();
    Ok(id.unwrap_or(Object::Int(0)))
}

fn zstddict_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let n = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_dict_content")))
        .and_then(|o| o.as_bytes_view())
        .map_or(0, |b| b.len());
    Ok(Object::Int(n as i64))
}

/// CPython `ZstdDict_repr`: `<ZstdDict dict_id=%u dict_size=%zd>`
/// (test_zstd test_train_dict / test_len).
fn zstddict_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let (dict_id, size) = {
        let d = inst.dict.borrow();
        let dict_id = d
            .get(&DictKey(Object::from_static("_dict_id")))
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let size = d
            .get(&DictKey(Object::from_static("_dict_content")))
            .and_then(Object::as_bytes_view)
            .map_or(0, |b| b.len());
        (dict_id, size)
    };
    Ok(Object::from_str(format!(
        "<ZstdDict dict_id={dict_id} dict_size={size}>"
    )))
}

fn zstddict_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        class_method_kw(&mut dict, "__init__", zstddict_init);
        class_method(&mut dict, "__len__", zstddict_len);
        class_method(&mut dict, "__repr__", zstddict_repr);
        // Read-only data: CPython's `dict_id` member and `dict_content`
        // getter (test_zstd test_is_raw asserts assignment fails).
        class_getter(&mut dict, "dict_content", zstddict_dict_content);
        class_getter(&mut dict, "dict_id", zstddict_dict_id);
        // The (dict, type) advice tuples, CPython's `dictionary_type`:
        // 0 = digested, 1 = undigested, 2 = prefix.
        class_getter(
            &mut dict,
            "as_digested_dict",
            zstddict_as_mode(DICT_TYPE_DIGESTED),
        );
        class_getter(
            &mut dict,
            "as_undigested_dict",
            zstddict_as_mode(DICT_TYPE_UNDIGESTED),
        );
        class_getter(&mut dict, "as_prefix", zstddict_as_mode(DICT_TYPE_PREFIX));
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

/// Argument Clinic `Py_ssize_t`: `__index__`, with a huge int an
/// OverflowError (test_zstd test_finalize_dict_c: `dict_size=2**1000`).
fn ssize_arg(arg: Option<&Object>, func: &str, name: &str) -> Result<i64, RuntimeError> {
    match arg {
        Some(o) => crate::builtins::coerce_index_i64(o),
        None => Err(type_error(format!(
            "{func}() missing required argument '{name}'"
        ))),
    }
}

/// Argument Clinic `object(subclass_of='&PyTuple_Type')`.
fn tuple_arg<'a>(
    arg: Option<&'a Object>,
    func: &str,
    pos: usize,
) -> Result<&'a [Object], RuntimeError> {
    match arg {
        Some(Object::Tuple(items)) => Ok(&items[..]),
        Some(other) => Err(type_error(format!(
            "{func}() argument {pos} must be tuple, not {}",
            other.type_name_owned()
        ))),
        None => Err(type_error(format!(
            "{func}() missing required argument {pos}"
        ))),
    }
}

/// CPython `calculate_samples_stats`: each size goes through
/// `PyLong_AsSize_t` (a non-int is a TypeError; a negative or oversized
/// one is an OverflowError that the function folds into the mismatch
/// ValueError), and the sizes must add up to the concatenation's length
/// exactly (test_zstd test_train_dict_c, test_train_buffer_protocol_samples).
fn sample_sizes(items: &[Object], total: usize) -> Result<Vec<usize>, RuntimeError> {
    let mismatch = || value_error("The samples size tuple doesn't match the concatenation's size.");
    let mut remaining = total;
    let mut sizes = Vec::with_capacity(items.len());
    for o in items {
        let size = match o {
            Object::Long(b) => {
                use num_traits::ToPrimitive;
                b.to_u64().and_then(|v| usize::try_from(v).ok())
            }
            _ if is_int_object(o) => o.as_i64().and_then(|v| usize::try_from(v).ok()),
            _ => return Err(type_error("an integer is required")),
        };
        let Some(size) = size else {
            return Err(mismatch());
        };
        if remaining < size {
            return Err(mismatch());
        }
        remaining -= size;
        sizes.push(size);
    }
    if remaining != 0 {
        return Err(mismatch());
    }
    Ok(sizes)
}

fn zdict_error(code: usize, what: &str) -> RuntimeError {
    let name = unsafe { std::ffi::CStr::from_ptr(zstd_sys::ZDICT_getErrorName(code)) }
        .to_string_lossy()
        .into_owned();
    zstd_error(format!("{what}: {name}"))
}

/// `train_dict(samples_bytes, samples_sizes, dict_size, /)`:
/// `ZDICT_trainFromBuffer` over the concatenated samples. Argument types
/// are Clinic-strict (`bytes`, `tuple`, `Py_ssize_t`), `dict_size` is
/// checked before the sizes, and a libzstd failure is a ZstdError
/// (test_zstd test_train_dict_c).
fn m_train_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 3 {
        return Err(type_error(format!(
            "train_dict() takes exactly 3 arguments ({} given)",
            args.len()
        )));
    }
    let chunks = exact_bytes_arg(args.first(), "train_dict", 1)?;
    let sizes_tuple = tuple_arg(args.get(1), "train_dict", 2)?;
    let dict_size = ssize_arg(args.get(2), "train_dict", "dict_size")?;
    if dict_size <= 0 {
        return Err(value_error("dict_size argument should be positive number."));
    }
    let sizes = sample_sizes(sizes_tuple, chunks.len())?;
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
        return Err(zdict_error(n, "Unable to train the Zstandard dictionary"));
    }
    buf.truncate(n);
    Ok(Object::new_bytes(buf))
}

/// `finalize_dict(custom_dict_bytes, samples_bytes, samples_sizes,
/// dict_size, compression_level, /)`: `ZDICT_finalizeDictionary`
/// (test_zstd test_finalize_dict_c).
fn m_finalize_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 5 {
        return Err(type_error(format!(
            "finalize_dict() takes exactly 5 arguments ({} given)",
            args.len()
        )));
    }
    let base = exact_bytes_arg(args.first(), "finalize_dict", 1)?;
    let chunks = exact_bytes_arg(args.get(1), "finalize_dict", 2)?;
    let sizes_tuple = tuple_arg(args.get(2), "finalize_dict", 3)?;
    let dict_size = ssize_arg(args.get(3), "finalize_dict", "dict_size")?;
    // Clinic `int`: `__index__` then a C `int` range check (OverflowError).
    let level = match args.get(4) {
        Some(o) => option_c_int(o)?,
        None => {
            return Err(type_error(
                "finalize_dict() missing required argument 'compression_level'",
            ))
        }
    };
    if dict_size <= 0 {
        return Err(value_error("dict_size argument should be positive number."));
    }
    let sizes = sample_sizes(sizes_tuple, chunks.len())?;
    let params = zstd_sys::ZDICT_params_t {
        compressionLevel: level,
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
        return Err(zdict_error(
            n,
            "Unable to finalize the Zstandard dictionary",
        ));
    }
    buf.truncate(n);
    Ok(Object::new_bytes(buf))
}

/// `get_frame_info(frame_buffer)` → `(decompressed_size | None, dict_id)`.
fn m_get_frame_info(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = buffer_arg(args.first())?;
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

/// `get_frame_size(frame_buffer)`: the compressed size of the first
/// complete frame. The failure text is CPython's
/// `_zstd_get_frame_size_impl` message (test_zstd test_get_frame_size
/// matches "not less than this complete frame").
fn m_get_frame_size(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = buffer_arg(args.first())?;
    let n = unsafe {
        zstd_sys::ZSTD_findFrameCompressedSize(data.as_ptr().cast::<c_void>(), data.len())
    };
    if unsafe { zstd_sys::ZSTD_isError(n) } != 0 {
        return Err(zstd_error(format!(
            "Error when finding the compressed size of a Zstandard frame. \
             Ensure the frame_buffer argument starts from the beginning of a \
             frame, and its length is not less than this complete frame. \
             Zstd error message: {}.",
            zstd_error_name(n)
        )));
    }
    Ok(Object::Int(n as i64))
}

/// `get_param_bounds(parameter, is_compress=True)` → `(lower, upper)`.
fn m_get_param_bounds(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let param = match args.first().or_else(|| kwarg(kwargs, "parameter")) {
        Some(o) => crate::builtins::coerce_index_i64(o)? as c_int,
        None => return Err(type_error("get_param_bounds requires a parameter")),
    };
    let is_compress = match args.get(1).or_else(|| kwarg(kwargs, "is_compress")) {
        Some(o) => o.is_truthy(),
        None => true,
    };
    let bounds = if is_compress {
        let (p, _) = cparam_from(i64::from(param)).ok_or_else(|| {
            zstd_error("Unable to get zstd compression parameter bounds: Unsupported parameter")
        })?;
        unsafe { zstd_sys::ZSTD_cParam_getBounds(p) }
    } else {
        let (p, _) = dparam_from(i64::from(param)).ok_or_else(|| {
            zstd_error("Unable to get zstd decompression parameter bounds: Unsupported parameter")
        })?;
        unsafe { zstd_sys::ZSTD_dParam_getBounds(p) }
    };
    check_zstd(
        bounds.error,
        if is_compress {
            "Unable to get zstd compression parameter bounds"
        } else {
            "Unable to get zstd decompression parameter bounds"
        },
    )?;
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(bounds.lowerBound)),
        Object::Int(i64::from(bounds.upperBound)),
    ]))
}

/// `set_parameter_types(c_parameter_type, d_parameter_type)`: CPython
/// records the two IntEnum types so the option-dict loops can reject a
/// key belonging to the other one (`_zstd_set_parameter_types_impl`).
fn m_set_parameter_types(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut types = Vec::with_capacity(2);
    for (pos, name) in [(0usize, "c_parameter_type"), (1, "d_parameter_type")] {
        match args.get(pos) {
            Some(Object::Type(t)) => types.push(t.clone()),
            Some(other) => {
                return Err(type_error(format!(
                    "set_parameter_types() argument '{name}' must be type, not {}",
                    other.type_name_owned()
                )))
            }
            None => {
                return Err(type_error(format!(
                    "set_parameter_types() missing required argument '{name}' (pos {})",
                    pos + 1
                )))
            }
        }
    }
    if let Ok(mut reg) = parameter_types().lock() {
        reg.d_parameter = types.pop();
        reg.c_parameter = types.pop();
    }
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
