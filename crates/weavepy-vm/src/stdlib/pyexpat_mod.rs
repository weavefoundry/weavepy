//! `pyexpat` — bindings to the real (vendored) expat 2.6.4 (RFC 0056 WS3).
//!
//! This replaces the earlier `quick-xml`-based approximation with a faithful
//! port of CPython's `Modules/pyexpat.c` over `vendor/expat-sys`: a true push
//! parser (incremental `Parse(data, isfinal)` chunks fire handlers as tokens
//! complete), the full 22-slot handler table, DTD/entity/notation/attlist
//! declaration events, namespace triplets, external-entity subparsers,
//! character-data buffering (`buffer_text`/`buffer_size`), interning, live
//! `Current*`/`Error*` position attributes and `GetInputContext`.
//!
//! Structure mirrors `_sqlite3` (`sqlite3_native`): native state lives in a
//! process-global registry keyed by an integer handle stored on the Python
//! instance's `_handle`; expat's C callbacks re-enter the VM through the
//! published interpreter pointer, and Python-level exceptions raised inside a
//! handler are parked in the state (`pending_exc`) while `XML_StopParser`
//! aborts the C-side parse — exactly CPython's `flag_error` protocol.

#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicI64, Ordering};

use expat_sys as ex;
use expat_sys::XML_Parser;

use crate::error::{
    attribute_error, overflow_error, recursion_error, type_error, value_error, PyException,
    RuntimeError,
};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::{PyInstance, TypeFlags, TypeObject};

// ---------------------------------------------------------------------------
// Interpreter re-entry (the `_sqlite3` pattern)
// ---------------------------------------------------------------------------

type Interp = crate::Interpreter;

fn interp<'a>() -> Result<&'a mut Interp, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| RuntimeError::Internal("pyexpat: no running interpreter".to_owned()))?;
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

fn call(ip: &mut Interp, f: &Object, args: &[Object]) -> Result<Object, RuntimeError> {
    let globals = ip.builtins_dict();
    ip.call_object_with_globals(f, args, &[], &globals)
}

// ---------------------------------------------------------------------------
// Handler slots (CPython's handler_info table)
// ---------------------------------------------------------------------------

const H_START_ELEMENT: usize = 0;
const H_END_ELEMENT: usize = 1;
const H_PROCESSING_INSTRUCTION: usize = 2;
const H_CHARACTER_DATA: usize = 3;
const H_UNPARSED_ENTITY_DECL: usize = 4;
const H_NOTATION_DECL: usize = 5;
const H_START_NAMESPACE_DECL: usize = 6;
const H_END_NAMESPACE_DECL: usize = 7;
const H_COMMENT: usize = 8;
const H_START_CDATA: usize = 9;
const H_END_CDATA: usize = 10;
const H_DEFAULT: usize = 11;
const H_DEFAULT_EXPAND: usize = 12;
const H_NOT_STANDALONE: usize = 13;
const H_EXTERNAL_ENTITY_REF: usize = 14;
const H_START_DOCTYPE_DECL: usize = 15;
const H_END_DOCTYPE_DECL: usize = 16;
const H_ENTITY_DECL: usize = 17;
const H_XML_DECL: usize = 18;
const H_ELEMENT_DECL: usize = 19;
const H_ATTLIST_DECL: usize = 20;
const H_SKIPPED_ENTITY: usize = 21;
const N_HANDLERS: usize = 22;

const HANDLER_NAMES: [&str; N_HANDLERS] = [
    "StartElementHandler",
    "EndElementHandler",
    "ProcessingInstructionHandler",
    "CharacterDataHandler",
    "UnparsedEntityDeclHandler",
    "NotationDeclHandler",
    "StartNamespaceDeclHandler",
    "EndNamespaceDeclHandler",
    "CommentHandler",
    "StartCdataSectionHandler",
    "EndCdataSectionHandler",
    "DefaultHandler",
    "DefaultHandlerExpand",
    "NotStandaloneHandler",
    "ExternalEntityRefHandler",
    "StartDoctypeDeclHandler",
    "EndDoctypeDeclHandler",
    "EntityDeclHandler",
    "XmlDeclHandler",
    "ElementDeclHandler",
    "AttlistDeclHandler",
    "SkippedEntityHandler",
];

fn slot_for(name: &str) -> Option<usize> {
    HANDLER_NAMES.iter().position(|n| *n == name)
}

// ---------------------------------------------------------------------------
// Parser state + registry
// ---------------------------------------------------------------------------

/// Default character-data buffer size (pyexpat.c `new_parser_object`).
const DEFAULT_BUFFER_SIZE: usize = 8192;

struct ExpatState {
    /// Raw `XML_Parser`, stored as usize (Send); 0 after free.
    parser: usize,
    /// Python handler objects (`Object::None` when unset), by slot.
    handlers: Vec<Object>,
    /// The string-interning dict (always a real Python dict).
    intern: Object,
    /// `buffer_text`: coalesce character data until the next non-chardata
    /// event (or the buffer fills up).
    buffer_text: bool,
    buffer: Vec<u8>,
    buffer_size: usize,
    ordered_attributes: bool,
    specified_attributes: bool,
    ns_prefixes: bool,
    /// Nesting depth of Python handler callbacks (drives `GetInputContext`).
    in_callback: u32,
    /// Python exception raised inside a handler; the parse is aborted and
    /// this is re-raised from `Parse`/`ParseFile`.
    pending_exc: Option<RuntimeError>,
    /// Tracked value for `GetReparseDeferralEnabled` (expat >= 2.6).
    reparse_deferral: bool,
    /// Strong ref to the parent parser instance for subparsers created by
    /// `ExternalEntityParserCreate`: expat subparsers use their parent's
    /// `XML_Parser` internals, so the parent must outlive them (gh-139400).
    /// Never read — its only job is the keepalive.
    #[allow(dead_code)]
    parent: Object,
}

impl ExpatState {
    fn parser(&self) -> XML_Parser {
        self.parser as XML_Parser
    }
}

type StateRef = Rc<RefCell<ExpatState>>;

fn parser_reg() -> &'static parking_lot::Mutex<std::collections::HashMap<i64, StateRef>> {
    static REG: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<i64, StateRef>>> =
        std::sync::OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

fn next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn state_of(id: i64) -> Option<StateRef> {
    parser_reg().lock().get(&id).cloned()
}

/// Registry lookup from an expat userdata pointer (the state id).
fn state_from_ud(ud: *mut c_void) -> Option<StateRef> {
    state_of(ud as i64)
}

fn self_inst(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("expected xmlparser instance")),
    }
}

fn state_of_args(args: &[Object]) -> Result<StateRef, RuntimeError> {
    let inst = self_inst(args)?;
    let handle = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned();
    match handle {
        Some(Object::Int(id)) => {
            state_of(id).ok_or_else(|| value_error("xmlparser has been freed"))
        }
        _ => Err(type_error("xmlparser instance missing _handle")),
    }
}

// ---------------------------------------------------------------------------
// C-string / interning conversions
// ---------------------------------------------------------------------------

/// Convert a NUL-terminated expat string (always valid UTF-8 in this build)
/// to an owned Rust string.
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(std::ffi::CStr::from_ptr(p).to_bytes()).into_owned()
}

/// NULL-tolerant conversion: `None` for NULL (CPython `STRING_CONV_FUNC`).
unsafe fn conv_opt(p: *const c_char) -> Object {
    if p.is_null() {
        Object::None
    } else {
        Object::from_str(cstr(p))
    }
}

unsafe fn conv_len(s: *const c_char, len: c_int) -> Object {
    if s.is_null() {
        return Object::None;
    }
    let bytes = std::slice::from_raw_parts(s.cast::<u8>(), len as usize);
    Object::from_str(String::from_utf8_lossy(bytes).into_owned())
}

/// `string_intern` (pyexpat.c): return the canonical `str` for `p` out of the
/// parser's intern dict, inserting on first sight. NULL converts to `None`.
unsafe fn intern_cstr(st: &StateRef, p: *const c_char) -> Object {
    if p.is_null() {
        return Object::None;
    }
    let s = cstr(p);
    let dict = st.borrow().intern.clone();
    if let Object::Dict(dd) = &dict {
        let key = DictKey(Object::from_str(s));
        if let Some(existing) = dd.borrow().get(&key).cloned() {
            return existing;
        }
        let obj = key.0.clone();
        dd.borrow_mut().insert(key, obj.clone());
        return obj;
    }
    key_fallback(s)
}

fn key_fallback(s: String) -> Object {
    Object::from_str(s)
}

// ---------------------------------------------------------------------------
// Handler dispatch + character-data buffering
// ---------------------------------------------------------------------------

/// Call the Python handler in `slot`. Returns `None` if the handler is unset,
/// an exception is already pending, or the handler raised (in which case the
/// exception is parked and the parse aborted — CPython's `flag_error`).
fn dispatch(st: &StateRef, slot: usize, args: Vec<Object>) -> Option<Object> {
    let handler = {
        let s = st.borrow();
        if s.pending_exc.is_some() {
            return None;
        }
        s.handlers[slot].clone()
    };
    if matches!(handler, Object::None) {
        return None;
    }
    let Ok(ip) = interp() else {
        return None;
    };
    st.borrow_mut().in_callback += 1;
    let result = call(ip, &handler, &args);
    st.borrow_mut().in_callback -= 1;
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            flag_error(st, e);
            None
        }
    }
}

fn flag_error(st: &StateRef, e: RuntimeError) {
    let parser = {
        let mut s = st.borrow_mut();
        if s.pending_exc.is_none() {
            s.pending_exc = Some(e);
        }
        s.parser()
    };
    // SAFETY: aborting a live parse; harmless (error return) outside one.
    unsafe {
        ex::XML_StopParser(parser, ex::XML_FALSE);
    }
}

/// `flush_character_buffer`: hand accumulated character data to the
/// CharacterDataHandler. Returns false when an exception is pending.
fn flush_chardata(st: &StateRef) -> bool {
    let (handler, bytes) = {
        let mut s = st.borrow_mut();
        if s.pending_exc.is_some() {
            return false;
        }
        if s.buffer.is_empty() {
            return true;
        }
        let bytes = std::mem::take(&mut s.buffer);
        (s.handlers[H_CHARACTER_DATA].clone(), bytes)
    };
    if matches!(handler, Object::None) {
        return true;
    }
    let text = Object::from_str(String::from_utf8_lossy(&bytes).into_owned());
    dispatch(st, H_CHARACTER_DATA, vec![text]);
    st.borrow().pending_exc.is_none()
}

// ---------------------------------------------------------------------------
// expat C trampolines
// ---------------------------------------------------------------------------

macro_rules! get_state {
    ($ud:expr) => {
        match state_from_ud($ud) {
            Some(st) => st,
            None => return,
        }
    };
}

unsafe extern "C" fn tr_start_element(
    ud: *mut c_void,
    name: *const c_char,
    atts: *mut *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let (ordered, specified, parser) = {
        let s = st.borrow();
        (s.ordered_attributes, s.specified_attributes, s.parser())
    };
    // Number of filled slots in atts[]; with specified_attributes only the
    // actually-specified (non-defaulted) leading slots are reported.
    let max = if specified {
        ex::XML_GetSpecifiedAttributeCount(parser) as usize
    } else {
        let mut n = 0usize;
        while !(*atts.add(n)).is_null() {
            n += 2;
        }
        n
    };
    let name_obj = intern_cstr(&st, name);
    let container = if ordered {
        let mut items = Vec::with_capacity(max);
        let mut i = 0usize;
        while i < max && !(*atts.add(i)).is_null() {
            items.push(intern_cstr(&st, *atts.add(i)));
            items.push(conv_opt(*atts.add(i + 1)));
            i += 2;
        }
        Object::List(Rc::new(RefCell::new(items)))
    } else {
        let dict = Rc::new(RefCell::new(DictData::default()));
        {
            let mut d = dict.borrow_mut();
            let mut i = 0usize;
            while i < max && !(*atts.add(i)).is_null() {
                d.insert(
                    DictKey(intern_cstr(&st, *atts.add(i))),
                    conv_opt(*atts.add(i + 1)),
                );
                i += 2;
            }
        }
        Object::Dict(dict)
    };
    dispatch(&st, H_START_ELEMENT, vec![name_obj, container]);
}

unsafe extern "C" fn tr_end_element(ud: *mut c_void, name: *const c_char) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let name_obj = intern_cstr(&st, name);
    dispatch(&st, H_END_ELEMENT, vec![name_obj]);
}

unsafe extern "C" fn tr_character_data(ud: *mut c_void, s: *const c_char, len: c_int) {
    let st = get_state!(ud);
    let data = std::slice::from_raw_parts(s.cast::<u8>(), len as usize);
    let (buffering, fits, oversize) = {
        let stb = st.borrow();
        if stb.pending_exc.is_some() {
            return;
        }
        let buffering = stb.buffer_text;
        let fits = stb.buffer.len() + data.len() <= stb.buffer_size;
        let oversize = data.len() > stb.buffer_size;
        (buffering, fits, oversize)
    };
    if !buffering {
        let text = Object::from_str(String::from_utf8_lossy(data).into_owned());
        dispatch(&st, H_CHARACTER_DATA, vec![text]);
        return;
    }
    if !fits {
        if !flush_chardata(&st) {
            return;
        }
        // The handler may have unset itself; drop the rest on the floor then
        // (pyexpat.c my_CharacterDataHandler).
        if matches!(st.borrow().handlers[H_CHARACTER_DATA], Object::None) {
            return;
        }
    }
    if oversize {
        let text = Object::from_str(String::from_utf8_lossy(data).into_owned());
        dispatch(&st, H_CHARACTER_DATA, vec![text]);
    } else {
        st.borrow_mut().buffer.extend_from_slice(data);
    }
}

unsafe extern "C" fn tr_processing_instruction(
    ud: *mut c_void,
    target: *const c_char,
    data: *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(
        &st,
        H_PROCESSING_INSTRUCTION,
        vec![conv_opt(target), conv_opt(data)],
    );
}

unsafe extern "C" fn tr_comment(ud: *mut c_void, data: *const c_char) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_COMMENT, vec![conv_opt(data)]);
}

unsafe extern "C" fn tr_start_cdata(ud: *mut c_void) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_START_CDATA, vec![]);
}

unsafe extern "C" fn tr_end_cdata(ud: *mut c_void) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_END_CDATA, vec![]);
}

unsafe extern "C" fn tr_default(ud: *mut c_void, s: *const c_char, len: c_int) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_DEFAULT, vec![conv_len(s, len)]);
}

unsafe extern "C" fn tr_default_expand(ud: *mut c_void, s: *const c_char, len: c_int) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_DEFAULT_EXPAND, vec![conv_len(s, len)]);
}

/// Convert a Python handler return value to a C int result (CPython's
/// `PyLong_AsLong` conversion in `RC_HANDLER`).
fn int_result(st: &StateRef, rv: Option<Object>) -> c_int {
    match rv {
        Some(Object::Int(i)) => i as c_int,
        Some(Object::Bool(b)) => c_int::from(b),
        Some(other) => {
            flag_error(
                st,
                type_error(format!(
                    "'{}' object cannot be interpreted as an integer",
                    other.type_name_owned()
                )),
            );
            0
        }
        None => 0,
    }
}

unsafe extern "C" fn tr_not_standalone(ud: *mut c_void) -> c_int {
    let Some(st) = state_from_ud(ud) else {
        return 0;
    };
    if !flush_chardata(&st) {
        return 0;
    }
    let rv = dispatch(&st, H_NOT_STANDALONE, vec![]);
    int_result(&st, rv)
}

unsafe extern "C" fn tr_external_entity_ref(
    parser: XML_Parser,
    context: *const c_char,
    base: *const c_char,
    system_id: *const c_char,
    public_id: *const c_char,
) -> c_int {
    // This is the one handler expat passes the parser (not userdata) to.
    let ud = ex::XML_GetUserData(parser);
    let Some(st) = state_from_ud(ud) else {
        return 0;
    };
    if !flush_chardata(&st) {
        return 0;
    }
    let args = vec![
        conv_opt(context),
        intern_cstr(&st, base),
        intern_cstr(&st, system_id),
        intern_cstr(&st, public_id),
    ];
    let rv = dispatch(&st, H_EXTERNAL_ENTITY_REF, args);
    int_result(&st, rv)
}

unsafe extern "C" fn tr_start_namespace_decl(
    ud: *mut c_void,
    prefix: *const c_char,
    uri: *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(
        &st,
        H_START_NAMESPACE_DECL,
        vec![conv_opt(prefix), conv_opt(uri)],
    );
}

unsafe extern "C" fn tr_end_namespace_decl(ud: *mut c_void, prefix: *const c_char) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_END_NAMESPACE_DECL, vec![conv_opt(prefix)]);
}

unsafe extern "C" fn tr_start_doctype_decl(
    ud: *mut c_void,
    doctype_name: *const c_char,
    sysid: *const c_char,
    pubid: *const c_char,
    has_internal_subset: c_int,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, doctype_name),
        intern_cstr(&st, sysid),
        intern_cstr(&st, pubid),
        Object::Int(i64::from(has_internal_subset)),
    ];
    dispatch(&st, H_START_DOCTYPE_DECL, args);
}

unsafe extern "C" fn tr_end_doctype_decl(ud: *mut c_void) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    dispatch(&st, H_END_DOCTYPE_DECL, vec![]);
}

unsafe extern "C" fn tr_entity_decl(
    ud: *mut c_void,
    entity_name: *const c_char,
    is_parameter_entity: c_int,
    value: *const c_char,
    value_length: c_int,
    base: *const c_char,
    system_id: *const c_char,
    public_id: *const c_char,
    notation_name: *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, entity_name),
        Object::Int(i64::from(is_parameter_entity)),
        conv_len(value, value_length),
        intern_cstr(&st, base),
        intern_cstr(&st, system_id),
        intern_cstr(&st, public_id),
        intern_cstr(&st, notation_name),
    ];
    dispatch(&st, H_ENTITY_DECL, args);
}

unsafe extern "C" fn tr_unparsed_entity_decl(
    ud: *mut c_void,
    entity_name: *const c_char,
    base: *const c_char,
    system_id: *const c_char,
    public_id: *const c_char,
    notation_name: *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, entity_name),
        intern_cstr(&st, base),
        intern_cstr(&st, system_id),
        intern_cstr(&st, public_id),
        intern_cstr(&st, notation_name),
    ];
    dispatch(&st, H_UNPARSED_ENTITY_DECL, args);
}

unsafe extern "C" fn tr_notation_decl(
    ud: *mut c_void,
    notation_name: *const c_char,
    base: *const c_char,
    system_id: *const c_char,
    public_id: *const c_char,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, notation_name),
        intern_cstr(&st, base),
        intern_cstr(&st, system_id),
        intern_cstr(&st, public_id),
    ];
    dispatch(&st, H_NOTATION_DECL, args);
}

unsafe extern "C" fn tr_xml_decl(
    ud: *mut c_void,
    version: *const c_char,
    encoding: *const c_char,
    standalone: c_int,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        conv_opt(version),
        conv_opt(encoding),
        Object::Int(i64::from(standalone)),
    ];
    dispatch(&st, H_XML_DECL, args);
}

/// Recursive `XML_Content` → tuple conversion (`conv_content_model`), with a
/// depth cap so a pathologically nested model raises `RecursionError`
/// instead of exhausting the native stack (gh-145986).
unsafe fn conv_content_model(
    model: *const ex::XML_Content,
    depth: usize,
) -> Result<Object, RuntimeError> {
    if depth > 5000 {
        return Err(recursion_error(
            "maximum recursion depth exceeded while converting expat content model",
        ));
    }
    let m = &*model;
    let mut children = Vec::with_capacity(m.numchildren as usize);
    for i in 0..m.numchildren as usize {
        children.push(conv_content_model(m.children.add(i), depth + 1)?);
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(m.type_)),
        Object::Int(i64::from(m.quant)),
        conv_opt(m.name),
        Object::new_tuple(children),
    ]))
}

unsafe extern "C" fn tr_element_decl(
    ud: *mut c_void,
    name: *const c_char,
    model: *mut ex::XML_Content,
) {
    let st = get_state!(ud);
    let parser = st.borrow().parser();
    let converted = if flush_chardata(&st) {
        conv_content_model(model, 0)
    } else {
        Err(RuntimeError::Internal("aborted".to_owned()))
    };
    // The model must be freed exactly once, whatever happened above.
    ex::XML_FreeContentModel(parser, model);
    match converted {
        Ok(model_obj) => {
            dispatch(&st, H_ELEMENT_DECL, vec![intern_cstr(&st, name), model_obj]);
        }
        Err(RuntimeError::Internal(_)) => {}
        Err(e) => flag_error(&st, e),
    }
}

unsafe extern "C" fn tr_attlist_decl(
    ud: *mut c_void,
    elname: *const c_char,
    attname: *const c_char,
    att_type: *const c_char,
    dflt: *const c_char,
    isrequired: c_int,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, elname),
        intern_cstr(&st, attname),
        conv_opt(att_type),
        conv_opt(dflt),
        Object::Int(i64::from(isrequired)),
    ];
    dispatch(&st, H_ATTLIST_DECL, args);
}

unsafe extern "C" fn tr_skipped_entity(
    ud: *mut c_void,
    entity_name: *const c_char,
    is_parameter_entity: c_int,
) {
    let st = get_state!(ud);
    if !flush_chardata(&st) {
        return;
    }
    let args = vec![
        intern_cstr(&st, entity_name),
        Object::Int(i64::from(is_parameter_entity)),
    ];
    dispatch(&st, H_SKIPPED_ENTITY, args);
}

/// `PyUnknownEncodingHandler` (pyexpat.c): resolve an encoding expat doesn't
/// know natively by decoding bytes 0..=255 with Python's codec machinery
/// ('replace' errors) and handing expat the byte -> codepoint map. This is
/// what makes single-byte documents (`encoding='iso8859'` etc.) parse.
unsafe extern "C" fn tr_unknown_encoding(
    data: *mut c_void,
    name: *const c_char,
    info: *mut ex::XML_Encoding,
) -> c_int {
    let st = state_from_ud(data);
    let Ok(ip) = interp() else {
        return ex::XML_STATUS_ERROR;
    };
    let enc_name = cstr(name);
    let template: Vec<u8> = (0u8..=255u8).collect();
    let bytes = Object::new_bytes(template);
    let decoded = ip.load_attr_public(&bytes, "decode").and_then(|d| {
        call(
            ip,
            &d,
            &[Object::from_str(enc_name), Object::from_static("replace")],
        )
    });
    let decoded = match decoded {
        Ok(v) => v,
        Err(e) => {
            if let Some(st) = &st {
                flag_error(st, e);
            }
            return ex::XML_STATUS_ERROR;
        }
    };
    let chars: Vec<u32> = match &decoded {
        Object::Str(s) => s.chars().map(|c| c as u32).collect(),
        Object::WStr(cps) => cps.to_vec(),
        _ => return ex::XML_STATUS_ERROR,
    };
    if chars.len() != 256 {
        if let Some(st) = &st {
            flag_error(st, value_error("multi-byte encodings are not supported"));
        }
        return ex::XML_STATUS_ERROR;
    }
    let info = &mut *info;
    for (i, ch) in chars.iter().enumerate() {
        // U+FFFD marks bytes the codec couldn't map.
        info.map[i] = if *ch == 0xFFFD { -1 } else { *ch as c_int };
    }
    info.data = std::ptr::null_mut();
    info.convert = None;
    info.release = None;
    ex::XML_STATUS_OK
}

/// (Un)register the native trampoline for a handler slot.
unsafe fn apply_native_handler(parser: XML_Parser, slot: usize, on: bool) {
    match slot {
        H_START_ELEMENT => {
            ex::XML_SetStartElementHandler(parser, on.then_some(tr_start_element as _))
        }
        H_END_ELEMENT => ex::XML_SetEndElementHandler(parser, on.then_some(tr_end_element as _)),
        H_PROCESSING_INSTRUCTION => ex::XML_SetProcessingInstructionHandler(
            parser,
            on.then_some(tr_processing_instruction as _),
        ),
        H_CHARACTER_DATA => {
            ex::XML_SetCharacterDataHandler(parser, on.then_some(tr_character_data as _))
        }
        H_UNPARSED_ENTITY_DECL => {
            ex::XML_SetUnparsedEntityDeclHandler(parser, on.then_some(tr_unparsed_entity_decl as _))
        }
        H_NOTATION_DECL => {
            ex::XML_SetNotationDeclHandler(parser, on.then_some(tr_notation_decl as _))
        }
        H_START_NAMESPACE_DECL => {
            ex::XML_SetStartNamespaceDeclHandler(parser, on.then_some(tr_start_namespace_decl as _))
        }
        H_END_NAMESPACE_DECL => {
            ex::XML_SetEndNamespaceDeclHandler(parser, on.then_some(tr_end_namespace_decl as _))
        }
        H_COMMENT => ex::XML_SetCommentHandler(parser, on.then_some(tr_comment as _)),
        H_START_CDATA => {
            ex::XML_SetStartCdataSectionHandler(parser, on.then_some(tr_start_cdata as _))
        }
        H_END_CDATA => ex::XML_SetEndCdataSectionHandler(parser, on.then_some(tr_end_cdata as _)),
        H_DEFAULT => ex::XML_SetDefaultHandler(parser, on.then_some(tr_default as _)),
        H_DEFAULT_EXPAND => {
            ex::XML_SetDefaultHandlerExpand(parser, on.then_some(tr_default_expand as _))
        }
        H_NOT_STANDALONE => {
            ex::XML_SetNotStandaloneHandler(parser, on.then_some(tr_not_standalone as _))
        }
        H_EXTERNAL_ENTITY_REF => {
            ex::XML_SetExternalEntityRefHandler(parser, on.then_some(tr_external_entity_ref as _))
        }
        H_START_DOCTYPE_DECL => {
            ex::XML_SetStartDoctypeDeclHandler(parser, on.then_some(tr_start_doctype_decl as _))
        }
        H_END_DOCTYPE_DECL => {
            ex::XML_SetEndDoctypeDeclHandler(parser, on.then_some(tr_end_doctype_decl as _))
        }
        H_ENTITY_DECL => ex::XML_SetEntityDeclHandler(parser, on.then_some(tr_entity_decl as _)),
        H_XML_DECL => ex::XML_SetXmlDeclHandler(parser, on.then_some(tr_xml_decl as _)),
        H_ELEMENT_DECL => ex::XML_SetElementDeclHandler(parser, on.then_some(tr_element_decl as _)),
        H_ATTLIST_DECL => ex::XML_SetAttlistDeclHandler(parser, on.then_some(tr_attlist_decl as _)),
        H_SKIPPED_ENTITY => {
            ex::XML_SetSkippedEntityHandler(parser, on.then_some(tr_skipped_entity as _))
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// ExpatError + `errors` / `model` submodules
// ---------------------------------------------------------------------------

fn expat_error_type() -> Rc<TypeObject> {
    static CLS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let parent = crate::builtin_types::builtin_types().exception.clone();
        let cls = TypeObject::new_exception("ExpatError", parent).expect("ExpatError class");
        crate::stdlib::io::set_type_module(&cls, "xml.parsers.expat");
        cls
    })
    .clone()
}

/// `XML_ErrorString` for `code`, or empty for out-of-range codes.
fn error_string_for(code: c_int) -> Option<String> {
    // SAFETY: XML_ErrorString returns a static string or NULL for any input.
    let p = unsafe { ex::XML_ErrorString(code) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { cstr(p) })
    }
}

/// Raise `ExpatError` for the parser's current error state (`set_error`).
fn set_error(st: &StateRef, code: c_int) -> RuntimeError {
    let parser = st.borrow().parser();
    // SAFETY: live parser handle. `XML_Size` is u64 on unix builds but u32 on
    // windows-gnu, so a lossless `From` conversion isn't portable here.
    #[allow(clippy::cast_lossless)]
    let (lineno, column) = unsafe {
        (
            ex::XML_GetCurrentLineNumber(parser) as i64,
            ex::XML_GetCurrentColumnNumber(parser) as i64,
        )
    };
    let msg = format!(
        "{}: line {}, column {}",
        error_string_for(code).unwrap_or_else(|| "unknown error".to_owned()),
        lineno,
        column
    );
    let cls = expat_error_type();
    let einst = PyInstance::new(cls);
    einst.slot_set("args", Object::new_tuple(vec![Object::from_str(msg)]));
    // `code`/`lineno`/`offset` are plain instance attributes in CPython's
    // pyexpat (`PyObject_SetAttrString`), so they stay in the dict.
    {
        let mut d = einst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("code")),
            Object::Int(i64::from(code)),
        );
        d.insert(DictKey(Object::from_static("lineno")), Object::Int(lineno));
        d.insert(DictKey(Object::from_static("offset")), Object::Int(column));
    }
    RuntimeError::PyException(PyException::new(Object::Instance(Rc::new(einst))))
}

/// Error-code names in `enum XML_Error` order (expat.h); index == code.
const ERROR_NAMES: &[&str] = &[
    "XML_ERROR_NONE", // unused (code 0)
    "XML_ERROR_NO_MEMORY",
    "XML_ERROR_SYNTAX",
    "XML_ERROR_NO_ELEMENTS",
    "XML_ERROR_INVALID_TOKEN",
    "XML_ERROR_UNCLOSED_TOKEN",
    "XML_ERROR_PARTIAL_CHAR",
    "XML_ERROR_TAG_MISMATCH",
    "XML_ERROR_DUPLICATE_ATTRIBUTE",
    "XML_ERROR_JUNK_AFTER_DOC_ELEMENT",
    "XML_ERROR_PARAM_ENTITY_REF",
    "XML_ERROR_UNDEFINED_ENTITY",
    "XML_ERROR_RECURSIVE_ENTITY_REF",
    "XML_ERROR_ASYNC_ENTITY",
    "XML_ERROR_BAD_CHAR_REF",
    "XML_ERROR_BINARY_ENTITY_REF",
    "XML_ERROR_ATTRIBUTE_EXTERNAL_ENTITY_REF",
    "XML_ERROR_MISPLACED_XML_PI",
    "XML_ERROR_UNKNOWN_ENCODING",
    "XML_ERROR_INCORRECT_ENCODING",
    "XML_ERROR_UNCLOSED_CDATA_SECTION",
    "XML_ERROR_EXTERNAL_ENTITY_HANDLING",
    "XML_ERROR_NOT_STANDALONE",
    "XML_ERROR_UNEXPECTED_STATE",
    "XML_ERROR_ENTITY_DECLARED_IN_PE",
    "XML_ERROR_FEATURE_REQUIRES_XML_DTD",
    "XML_ERROR_CANT_CHANGE_FEATURE_ONCE_PARSING",
    "XML_ERROR_UNBOUND_PREFIX",
    "XML_ERROR_UNDECLARING_PREFIX",
    "XML_ERROR_INCOMPLETE_PE",
    "XML_ERROR_XML_DECL",
    "XML_ERROR_TEXT_DECL",
    "XML_ERROR_PUBLICID",
    "XML_ERROR_SUSPENDED",
    "XML_ERROR_NOT_SUSPENDED",
    "XML_ERROR_ABORTED",
    "XML_ERROR_FINISHED",
    "XML_ERROR_SUSPEND_PE",
    "XML_ERROR_RESERVED_PREFIX_XML",
    "XML_ERROR_RESERVED_PREFIX_XMLNS",
    "XML_ERROR_RESERVED_NAMESPACE_URI",
    "XML_ERROR_INVALID_ARGUMENT",
    "XML_ERROR_NO_BUFFER",
    "XML_ERROR_AMPLIFICATION_LIMIT_BREACH",
    "XML_ERROR_NOT_STARTED",
];

fn errors_submodule() -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("xml.parsers.expat.errors"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Constants used to describe error conditions."),
        );
        // `codes`: message -> code; `messages`: code -> message; and one
        // module attribute per error name holding the message string.
        let codes = Rc::new(RefCell::new(DictData::default()));
        let messages = Rc::new(RefCell::new(DictData::default()));
        {
            let mut c = codes.borrow_mut();
            let mut m = messages.borrow_mut();
            for (code, name) in ERROR_NAMES.iter().enumerate().skip(1) {
                let Some(msg) = error_string_for(code as c_int) else {
                    continue;
                };
                d.insert(
                    DictKey(Object::from_static(name)),
                    Object::from_str(msg.clone()),
                );
                c.insert(
                    DictKey(Object::from_str(msg.clone())),
                    Object::Int(code as i64),
                );
                m.insert(DictKey(Object::Int(code as i64)), Object::from_str(msg));
            }
        }
        d.insert(DictKey(Object::from_static("codes")), Object::Dict(codes));
        d.insert(
            DictKey(Object::from_static("messages")),
            Object::Dict(messages),
        );
    }
    Rc::new(PyModule {
        name: "xml.parsers.expat.errors".to_owned(),
        filename: None,
        dict,
    })
}

fn model_submodule() -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("xml.parsers.expat.model"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Constants used to interpret content model information."),
        );
        for (name, val) in [
            ("XML_CTYPE_EMPTY", ex::XML_CTYPE_EMPTY),
            ("XML_CTYPE_ANY", ex::XML_CTYPE_ANY),
            ("XML_CTYPE_MIXED", ex::XML_CTYPE_MIXED),
            ("XML_CTYPE_NAME", ex::XML_CTYPE_NAME),
            ("XML_CTYPE_CHOICE", ex::XML_CTYPE_CHOICE),
            ("XML_CTYPE_SEQ", ex::XML_CTYPE_SEQ),
            ("XML_CQUANT_NONE", ex::XML_CQUANT_NONE),
            ("XML_CQUANT_OPT", ex::XML_CQUANT_OPT),
            ("XML_CQUANT_REP", ex::XML_CQUANT_REP),
            ("XML_CQUANT_PLUS", ex::XML_CQUANT_PLUS),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(val)),
            );
        }
    }
    Rc::new(PyModule {
        name: "xml.parsers.expat.model".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// xmlparser type
// ---------------------------------------------------------------------------

fn method(
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

fn getset(
    cls: &Rc<TypeObject>,
    name: &'static str,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    crate::stdlib::sqlite3_native::install_getset(cls, name, getter, None);
}

fn handler_get<const SLOT: usize>(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let h = st.borrow().handlers[SLOT].clone();
    Ok(h)
}

fn parser_type() -> Rc<TypeObject> {
    static CLS: std::sync::OnceLock<Rc<TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let mut d = DictData::default();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("pyexpat"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("XML parser"),
        );
        method(&mut d, "Parse", parse_method);
        method(&mut d, "ParseFile", parse_file_method);
        method(&mut d, "SetBase", set_base_method);
        method(&mut d, "GetBase", get_base_method);
        method(&mut d, "GetInputContext", get_input_context_method);
        method(
            &mut d,
            "SetParamEntityParsing",
            set_param_entity_parsing_method,
        );
        method(&mut d, "UseForeignDTD", use_foreign_dtd_method);
        method(
            &mut d,
            "ExternalEntityParserCreate",
            external_entity_parser_create,
        );
        method(
            &mut d,
            "GetReparseDeferralEnabled",
            get_reparse_deferral_method,
        );
        method(
            &mut d,
            "SetReparseDeferralEnabled",
            set_reparse_deferral_method,
        );
        method(&mut d, "__setattr__", setattr_method);
        method(&mut d, "__del__", del_method);
        let cls = TypeObject::new_with_flags(
            "xmlparser",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("xmlparser type");

        // Handler attributes (read side; writes go through __setattr__).
        getset(&cls, "StartElementHandler", handler_get::<H_START_ELEMENT>);
        getset(&cls, "EndElementHandler", handler_get::<H_END_ELEMENT>);
        getset(
            &cls,
            "ProcessingInstructionHandler",
            handler_get::<H_PROCESSING_INSTRUCTION>,
        );
        getset(
            &cls,
            "CharacterDataHandler",
            handler_get::<H_CHARACTER_DATA>,
        );
        getset(
            &cls,
            "UnparsedEntityDeclHandler",
            handler_get::<H_UNPARSED_ENTITY_DECL>,
        );
        getset(&cls, "NotationDeclHandler", handler_get::<H_NOTATION_DECL>);
        getset(
            &cls,
            "StartNamespaceDeclHandler",
            handler_get::<H_START_NAMESPACE_DECL>,
        );
        getset(
            &cls,
            "EndNamespaceDeclHandler",
            handler_get::<H_END_NAMESPACE_DECL>,
        );
        getset(&cls, "CommentHandler", handler_get::<H_COMMENT>);
        getset(
            &cls,
            "StartCdataSectionHandler",
            handler_get::<H_START_CDATA>,
        );
        getset(&cls, "EndCdataSectionHandler", handler_get::<H_END_CDATA>);
        getset(&cls, "DefaultHandler", handler_get::<H_DEFAULT>);
        getset(
            &cls,
            "DefaultHandlerExpand",
            handler_get::<H_DEFAULT_EXPAND>,
        );
        getset(
            &cls,
            "NotStandaloneHandler",
            handler_get::<H_NOT_STANDALONE>,
        );
        getset(
            &cls,
            "ExternalEntityRefHandler",
            handler_get::<H_EXTERNAL_ENTITY_REF>,
        );
        getset(
            &cls,
            "StartDoctypeDeclHandler",
            handler_get::<H_START_DOCTYPE_DECL>,
        );
        getset(
            &cls,
            "EndDoctypeDeclHandler",
            handler_get::<H_END_DOCTYPE_DECL>,
        );
        getset(&cls, "EntityDeclHandler", handler_get::<H_ENTITY_DECL>);
        getset(&cls, "XmlDeclHandler", handler_get::<H_XML_DECL>);
        getset(&cls, "ElementDeclHandler", handler_get::<H_ELEMENT_DECL>);
        getset(&cls, "AttlistDeclHandler", handler_get::<H_ATTLIST_DECL>);
        getset(
            &cls,
            "SkippedEntityHandler",
            handler_get::<H_SKIPPED_ENTITY>,
        );

        // Flags, buffering, interning.
        getset(&cls, "buffer_text", |args| {
            Ok(Object::Bool(state_of_args(args)?.borrow().buffer_text))
        });
        getset(&cls, "buffer_size", |args| {
            Ok(Object::Int(
                state_of_args(args)?.borrow().buffer_size as i64,
            ))
        });
        getset(&cls, "buffer_used", |args| {
            Ok(Object::Int(
                state_of_args(args)?.borrow().buffer.len() as i64
            ))
        });
        getset(&cls, "ordered_attributes", |args| {
            Ok(Object::Bool(
                state_of_args(args)?.borrow().ordered_attributes,
            ))
        });
        getset(&cls, "specified_attributes", |args| {
            Ok(Object::Bool(
                state_of_args(args)?.borrow().specified_attributes,
            ))
        });
        getset(&cls, "namespace_prefixes", |args| {
            Ok(Object::Bool(state_of_args(args)?.borrow().ns_prefixes))
        });
        getset(&cls, "intern", |args| {
            Ok(state_of_args(args)?.borrow().intern.clone())
        });

        // Live position / error attributes (pyexpat.c getsets).
        getset(&cls, "CurrentLineNumber", |args| {
            let p = state_of_args(args)?.borrow().parser();
            // `XML_Size` is u64 on unix builds but u32 on windows-gnu.
            #[allow(clippy::cast_lossless)]
            let line = unsafe { ex::XML_GetCurrentLineNumber(p) } as i64;
            Ok(Object::Int(line))
        });
        getset(&cls, "CurrentColumnNumber", |args| {
            let p = state_of_args(args)?.borrow().parser();
            #[allow(clippy::cast_lossless)] // XML_Size width differs per platform
            let col = unsafe { ex::XML_GetCurrentColumnNumber(p) } as i64;
            Ok(Object::Int(col))
        });
        getset(&cls, "CurrentByteIndex", |args| {
            let p = state_of_args(args)?.borrow().parser();
            // `XML_Index` is c_long: i64 on unix hosts, i32 on windows-gnu.
            #[allow(clippy::cast_lossless, clippy::unnecessary_cast)]
            let idx = unsafe { ex::XML_GetCurrentByteIndex(p) } as i64;
            Ok(Object::Int(idx))
        });
        getset(&cls, "ErrorCode", |args| {
            let p = state_of_args(args)?.borrow().parser();
            Ok(Object::Int(i64::from(unsafe { ex::XML_GetErrorCode(p) })))
        });
        getset(&cls, "ErrorLineNumber", |args| {
            let p = state_of_args(args)?.borrow().parser();
            #[allow(clippy::cast_lossless)] // XML_Size width differs per platform
            let line = unsafe { ex::XML_GetCurrentLineNumber(p) } as i64;
            Ok(Object::Int(line))
        });
        getset(&cls, "ErrorColumnNumber", |args| {
            let p = state_of_args(args)?.borrow().parser();
            #[allow(clippy::cast_lossless)] // XML_Size width differs per platform
            let col = unsafe { ex::XML_GetCurrentColumnNumber(p) } as i64;
            Ok(Object::Int(col))
        });
        getset(&cls, "ErrorByteIndex", |args| {
            let p = state_of_args(args)?.borrow().parser();
            #[allow(clippy::cast_lossless, clippy::unnecessary_cast)] // c_long width differs
            let idx = unsafe { ex::XML_GetCurrentByteIndex(p) } as i64;
            Ok(Object::Int(idx))
        });
        cls
    })
    .clone()
}

// ---------------------------------------------------------------------------
// __setattr__ / __del__
// ---------------------------------------------------------------------------

fn setattr_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let name = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "attribute name must be string, not '{}'",
                other.type_name_owned()
            )))
        }
        None => return Err(type_error("__setattr__ expected 2 arguments")),
    };
    let value = args.get(2).cloned().unwrap_or(Object::None);

    if let Some(slot) = slot_for(&name) {
        // Changing the CharacterDataHandler flushes pending data through the
        // *old* handler first (pyexpat.c xmlparse_handler_setter).
        if slot == H_CHARACTER_DATA && !flush_chardata(&st) {
            let e = st.borrow_mut().pending_exc.take();
            if let Some(e) = e {
                return Err(e);
            }
        }
        let on = !matches!(value, Object::None);
        let parser = {
            let mut s = st.borrow_mut();
            s.handlers[slot] = value;
            s.parser()
        };
        // SAFETY: live parser handle.
        unsafe { apply_native_handler(parser, slot, on) };
        return Ok(Object::None);
    }

    match name.as_str() {
        "buffer_text" => {
            let enable = value.is_truthy();
            let was = st.borrow().buffer_text;
            if was && !enable && !flush_chardata(&st) {
                let e = st.borrow_mut().pending_exc.take();
                if let Some(e) = e {
                    return Err(e);
                }
            }
            st.borrow_mut().buffer_text = enable;
        }
        "buffer_size" => {
            let new_size = match &value {
                Object::Int(i) => *i,
                Object::Bool(b) => i64::from(*b),
                Object::Long(_) => {
                    return Err(overflow_error("Python int too large to convert to C long"))
                }
                _ => return Err(type_error("buffer_size must be an integer")),
            };
            if new_size <= 0 {
                return Err(value_error("buffer_size must be greater than zero"));
            }
            let changed = st.borrow().buffer_size != new_size as usize;
            if changed {
                if !flush_chardata(&st) {
                    let e = st.borrow_mut().pending_exc.take();
                    if let Some(e) = e {
                        return Err(e);
                    }
                }
                st.borrow_mut().buffer_size = new_size as usize;
            }
        }
        "ordered_attributes" => st.borrow_mut().ordered_attributes = value.is_truthy(),
        "specified_attributes" => st.borrow_mut().specified_attributes = value.is_truthy(),
        "namespace_prefixes" => {
            let enable = value.is_truthy();
            let parser = {
                let mut s = st.borrow_mut();
                s.ns_prefixes = enable;
                s.parser()
            };
            // SAFETY: live parser handle.
            unsafe { ex::XML_SetReturnNSTriplet(parser, c_int::from(enable)) };
        }
        "intern"
        | "buffer_used"
        | "CurrentLineNumber"
        | "CurrentColumnNumber"
        | "CurrentByteIndex"
        | "ErrorCode"
        | "ErrorLineNumber"
        | "ErrorColumnNumber"
        | "ErrorByteIndex" => {
            return Err(attribute_error(format!(
                "attribute '{name}' of 'pyexpat.xmlparser' objects is not writable"
            )))
        }
        _ => {
            return Err(attribute_error(format!(
                "'xmlparser' object has no attribute '{name}'"
            )))
        }
    }
    Ok(Object::None)
}

fn del_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let handle = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned();
    if let Some(Object::Int(id)) = handle {
        if let Some(st) = parser_reg().lock().remove(&id) {
            let parser = st.borrow().parser();
            if !parser.is_null() {
                st.borrow_mut().parser = 0;
                // SAFETY: sole owner; nobody can reach this handle anymore.
                unsafe { ex::XML_ParserFree(parser) };
            }
        }
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// ParserCreate / ExternalEntityParserCreate
// ---------------------------------------------------------------------------

fn register_parser(
    parser: XML_Parser,
    intern: Object,
    ns_prefixes: bool,
    parent: Object,
) -> Object {
    let id = next_id();
    // SAFETY: fresh live parser.
    unsafe {
        ex::XML_SetUserData(parser, id as *mut c_void);
        ex::XML_SetUnknownEncodingHandler(parser, Some(tr_unknown_encoding), id as *mut c_void);
    }
    let state = Rc::new(RefCell::new(ExpatState {
        parser: parser as usize,
        handlers: vec![Object::None; N_HANDLERS],
        intern,
        buffer_text: false,
        buffer: Vec::new(),
        buffer_size: DEFAULT_BUFFER_SIZE,
        ordered_attributes: false,
        specified_attributes: false,
        ns_prefixes,
        in_callback: 0,
        pending_exc: None,
        reparse_deferral: expat_at_least(2, 6),
        parent,
    }));
    parser_reg().lock().insert(id, state);
    let inst = PyInstance::new(parser_type());
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    Object::Instance(Rc::new(inst))
}

fn expat_at_least(major: c_int, minor: c_int) -> bool {
    // SAFETY: pure struct-by-value query.
    let v = unsafe { ex::XML_ExpatVersionInfo() };
    (v.major, v.minor) >= (major, minor)
}

fn opt_str_arg(func: &str, name: &str, v: Option<&Object>) -> Result<Option<String>, RuntimeError> {
    match v {
        None | Some(Object::None) => Ok(None),
        Some(Object::Str(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(type_error(format!(
            "{func}() argument '{name}' must be str or None, not {}",
            other.type_name_owned()
        ))),
    }
}

fn parser_create(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mut encoding_arg = args.first().cloned();
    let mut sep_arg = args.get(1).cloned();
    let mut intern_arg = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "encoding" => encoding_arg = Some(v.clone()),
            "namespace_separator" => sep_arg = Some(v.clone()),
            "intern" => intern_arg = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "ParserCreate() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let encoding = opt_str_arg("ParserCreate", "encoding", encoding_arg.as_ref())?;
    let namespace_sep = match sep_arg {
        None | Some(Object::None) => None,
        Some(Object::Str(s)) => Some(s.to_string()),
        Some(other) => {
            return Err(type_error(format!(
                "ParserCreate() argument 'namespace_separator' must be str or None, not {}",
                other.type_name_owned()
            )))
        }
    };
    if let Some(s) = &namespace_sep {
        if s.chars().count() > 1 {
            return Err(value_error(
                "namespace_separator must be at most one character, omitted, or None",
            ));
        }
    }
    let intern = match intern_arg {
        None | Some(Object::None) => Object::Dict(Rc::new(RefCell::new(DictData::default()))),
        Some(d @ Object::Dict(_)) => d,
        Some(_) => return Err(type_error("intern must be a dictionary")),
    };

    let enc_c = encoding
        .map(|e| std::ffi::CString::new(e).map_err(|_| value_error("embedded null character")))
        .transpose()?;
    let enc_ptr = enc_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: valid (or null) encoding pointer; single-byte separator.
    let parser = unsafe {
        match &namespace_sep {
            Some(s) => {
                let sep = s.as_bytes().first().copied().unwrap_or(0) as c_char;
                ex::XML_ParserCreateNS(enc_ptr, sep)
            }
            None => ex::XML_ParserCreate(enc_ptr),
        }
    };
    if parser.is_null() {
        return Err(RuntimeError::PyException(PyException::from_builtin(
            "MemoryError",
            "",
        )));
    }
    Ok(register_parser(parser, intern, false, Object::None))
}

fn external_entity_parser_create(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let context = opt_str_arg("ExternalEntityParserCreate", "context", args.get(1))?;
    let encoding = opt_str_arg("ExternalEntityParserCreate", "encoding", args.get(2))?;

    let ctx_c = context
        .map(|c| std::ffi::CString::new(c).map_err(|_| value_error("embedded null character")))
        .transpose()?;
    let enc_c = encoding
        .map(|e| std::ffi::CString::new(e).map_err(|_| value_error("embedded null character")))
        .transpose()?;
    let ctx_ptr = ctx_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let enc_ptr = enc_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

    let (parent_parser, handlers, intern, buffer_text, buffer_size, ordered, specified, nsp) = {
        let s = st.borrow();
        (
            s.parser(),
            s.handlers.clone(),
            s.intern.clone(),
            s.buffer_text,
            s.buffer_size,
            s.ordered_attributes,
            s.specified_attributes,
            s.ns_prefixes,
        )
    };
    // SAFETY: live parent parser; context/encoding pointers valid or null.
    let child = unsafe { ex::XML_ExternalEntityParserCreate(parent_parser, ctx_ptr, enc_ptr) };
    if child.is_null() {
        return Err(RuntimeError::PyException(PyException::from_builtin(
            "MemoryError",
            "",
        )));
    }
    let parent_obj = args[0].clone();
    let obj = register_parser(child, intern, nsp, parent_obj);
    // Inherit the parent's Python-side configuration + handler table
    // (pyexpat.c ExternalEntityParserCreate).
    if let Object::Instance(inst) = &obj {
        let handle = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_handle")))
            .cloned();
        if let Some(Object::Int(id)) = handle {
            if let Some(child_st) = state_of(id) {
                {
                    let mut s = child_st.borrow_mut();
                    s.buffer_text = buffer_text;
                    s.buffer_size = buffer_size;
                    s.ordered_attributes = ordered;
                    s.specified_attributes = specified;
                    s.handlers = handlers;
                }
                let s = child_st.borrow();
                for (slot, h) in s.handlers.iter().enumerate() {
                    if !matches!(h, Object::None) {
                        // SAFETY: live child parser.
                        unsafe { apply_native_handler(child, slot, true) };
                    }
                }
                if nsp {
                    // SAFETY: live child parser.
                    unsafe { ex::XML_SetReturnNSTriplet(child, 1) };
                }
            }
        }
    }
    Ok(obj)
}

// ---------------------------------------------------------------------------
// Parse / ParseFile
// ---------------------------------------------------------------------------

/// Feed one chunk to expat and translate the outcome: pending Python
/// exception > ExpatError > success (returns expat's status code, 1).
fn feed(st: &StateRef, data: &[u8], isfinal: bool) -> Result<i64, RuntimeError> {
    let parser = st.borrow().parser();
    // SAFETY: live parser; `data` outlives the call.
    let rc = unsafe {
        ex::XML_Parse(
            parser,
            data.as_ptr().cast::<c_char>(),
            data.len() as c_int,
            c_int::from(isfinal),
        )
    };
    let pending = st.borrow_mut().pending_exc.take();
    if let Some(e) = pending {
        return Err(e);
    }
    if rc == ex::XML_STATUS_ERROR {
        let code = unsafe { ex::XML_GetErrorCode(parser) };
        return Err(set_error(st, code));
    }
    Ok(i64::from(rc))
}

fn parse_data_arg(st: &StateRef, arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Str(s)) => {
            // A str argument overrides any declared document encoding: the
            // data is fed as UTF-8 (pyexpat.c Parse).
            let parser = st.borrow().parser();
            // SAFETY: live parser; static encoding name.
            unsafe { ex::XML_SetEncoding(parser, c"utf-8".as_ptr()) };
            Ok(s.as_bytes().to_vec())
        }
        Some(Object::WStr(_)) => Err(RuntimeError::PyException(PyException::from_builtin(
            "UnicodeEncodeError",
            "'utf-8' codec can't encode surrogates",
        ))),
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::MemoryView(mv)) => {
            if mv.is_c_contiguous() {
                Ok(mv.to_bytes())
            } else {
                Err(RuntimeError::PyException(PyException::from_builtin(
                    "BufferError",
                    "underlying buffer is not C-contiguous",
                )))
            }
        }
        Some(other) => Err(type_error(format!(
            "a bytes-like object is required, not '{}'",
            other.type_name_owned()
        ))),
        None => Err(type_error(
            "Parse() missing 1 required positional argument: 'data'",
        )),
    }
}

fn parse_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let data = parse_data_arg(&st, args.get(1))?;
    let isfinal = args.get(2).map(Object::is_truthy).unwrap_or(false);
    let rc = feed(&st, &data, isfinal)?;
    if !flush_chardata(&st) {
        let e = st.borrow_mut().pending_exc.take();
        if let Some(e) = e {
            return Err(e);
        }
    }
    Ok(Object::Int(rc))
}

fn parse_file_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let file = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("ParseFile() missing 1 required positional argument: 'file'"))?;
    let ip = interp()?;
    let read = ip.load_attr_public(&file, "read")?;
    let rc;
    loop {
        let chunk = call(ip, &read, &[Object::Int(2048)])?;
        let bytes = match &chunk {
            Object::Bytes(b) => b.to_vec(),
            other => {
                return Err(type_error(format!(
                    "read() did not return a bytes object (type={})",
                    other.type_name_owned()
                )))
            }
        };
        let isfinal = bytes.is_empty();
        let status = feed(&st, &bytes, isfinal)?;
        if isfinal {
            rc = status;
            break;
        }
    }
    if !flush_chardata(&st) {
        let e = st.borrow_mut().pending_exc.take();
        if let Some(e) = e {
            return Err(e);
        }
    }
    Ok(Object::Int(rc))
}

// ---------------------------------------------------------------------------
// Remaining methods
// ---------------------------------------------------------------------------

fn set_base_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let base = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "SetBase() argument 'base' must be str, not {}",
                other.type_name_owned()
            )))
        }
        None => return Err(type_error("SetBase() missing 1 required argument: 'base'")),
    };
    let c = std::ffi::CString::new(base).map_err(|_| value_error("embedded null character"))?;
    let parser = st.borrow().parser();
    // SAFETY: live parser; expat copies the string.
    let ok = unsafe { ex::XML_SetBase(parser, c.as_ptr()) };
    if ok == 0 {
        return Err(RuntimeError::PyException(PyException::from_builtin(
            "MemoryError",
            "",
        )));
    }
    Ok(Object::None)
}

fn get_base_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let parser = st.borrow().parser();
    // SAFETY: live parser.
    Ok(unsafe { conv_opt(ex::XML_GetBase(parser)) })
}

fn get_input_context_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let (parser, in_callback) = {
        let s = st.borrow();
        (s.parser(), s.in_callback > 0)
    };
    if !in_callback {
        return Ok(Object::None);
    }
    let mut offset: c_int = 0;
    let mut size: c_int = 0;
    // SAFETY: live parser; out-params are stack locals.
    let buf = unsafe { ex::XML_GetInputContext(parser, &raw mut offset, &raw mut size) };
    if buf.is_null() {
        return Ok(Object::None);
    }
    // SAFETY: expat guarantees `buf[offset..size]` is readable here.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            buf.add(offset as usize).cast::<u8>(),
            (size - offset) as usize,
        )
    };
    Ok(Object::new_bytes(bytes.to_vec()))
}

fn set_param_entity_parsing_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let flag = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("an integer is required"))?;
    let parser = st.borrow().parser();
    // SAFETY: live parser.
    let rc = unsafe { ex::XML_SetParamEntityParsing(parser, flag as c_int) };
    Ok(Object::Int(i64::from(rc)))
}

fn use_foreign_dtd_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let flag = args.get(1).map(Object::is_truthy).unwrap_or(true);
    let parser = st.borrow().parser();
    // SAFETY: live parser.
    let rc = unsafe { ex::XML_UseForeignDTD(parser, ex::XML_Bool::from(flag)) };
    if rc != 0 {
        return Err(set_error(&st, rc));
    }
    Ok(Object::None)
}

fn get_reparse_deferral_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let v = st.borrow().reparse_deferral;
    Ok(Object::Bool(v))
}

fn set_reparse_deferral_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let st = state_of_args(args)?;
    let enabled = args.get(1).map(Object::is_truthy).unwrap_or(false);
    if expat_at_least(2, 6) {
        let parser = st.borrow().parser();
        // SAFETY: live parser.
        let ok = unsafe { ex::XML_SetReparseDeferralEnabled(parser, ex::XML_Bool::from(enabled)) };
        if ok != 0 {
            st.borrow_mut().reparse_deferral = enabled;
        }
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// Module-level functions + module construction
// ---------------------------------------------------------------------------

fn error_string_fn(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = args.first().and_then(Object::as_i64).unwrap_or(0);
    match error_string_for(code as c_int) {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("pyexpat"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Python wrapper for Expat parser."),
        );
        // SAFETY: pure static queries.
        let (version_str, vinfo) =
            unsafe { (cstr(ex::XML_ExpatVersion()), ex::XML_ExpatVersionInfo()) };
        d.insert(
            DictKey(Object::from_static("EXPAT_VERSION")),
            Object::from_str(version_str),
        );
        d.insert(
            DictKey(Object::from_static("version_info")),
            Object::new_tuple(vec![
                Object::Int(i64::from(vinfo.major)),
                Object::Int(i64::from(vinfo.minor)),
                Object::Int(i64::from(vinfo.micro)),
            ]),
        );
        d.insert(
            DictKey(Object::from_static("native_encoding")),
            Object::from_static("UTF-8"),
        );
        let mut features = Vec::new();
        // SAFETY: static feature table terminated by a NULL-named entry.
        unsafe {
            let mut f = ex::XML_GetFeatureList();
            while !f.is_null() && !(*f).name.is_null() && (*f).feature != 0 {
                // Feature values are c_long: i64 on unix hosts, i32 on windows-gnu.
                #[allow(clippy::cast_lossless, clippy::unnecessary_cast)]
                let value = (*f).value as i64;
                features.push(Object::new_tuple(vec![
                    Object::from_str(cstr((*f).name)),
                    Object::Int(value),
                ]));
                f = f.add(1);
            }
        }
        d.insert(
            DictKey(Object::from_static("features")),
            Object::List(Rc::new(RefCell::new(features))),
        );
        d.insert(
            DictKey(Object::from_static("XML_PARAM_ENTITY_PARSING_NEVER")),
            Object::Int(i64::from(ex::XML_PARAM_ENTITY_PARSING_NEVER)),
        );
        d.insert(
            DictKey(Object::from_static(
                "XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE",
            )),
            Object::Int(i64::from(ex::XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE)),
        );
        d.insert(
            DictKey(Object::from_static("XML_PARAM_ENTITY_PARSING_ALWAYS")),
            Object::Int(i64::from(ex::XML_PARAM_ENTITY_PARSING_ALWAYS)),
        );
        d.insert(
            DictKey(Object::from_static("ParserCreate")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "ParserCreate",
                binds_instance: false,
                call: Box::new(move |args| parser_create(args, &[])),
                call_kw: Some(Box::new(parser_create)),
            })),
        );
        d.insert(
            DictKey(Object::from_static("ErrorString")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "ErrorString",
                binds_instance: false,
                call: Box::new(error_string_fn),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("XMLParserType")),
            Object::Type(parser_type()),
        );
        let err = expat_error_type();
        d.insert(
            DictKey(Object::from_static("ExpatError")),
            Object::Type(err.clone()),
        );
        d.insert(DictKey(Object::from_static("error")), Object::Type(err));
        d.insert(
            DictKey(Object::from_static("errors")),
            Object::Module(errors_submodule()),
        );
        d.insert(
            DictKey(Object::from_static("model")),
            Object::Module(model_submodule()),
        );
    }
    Rc::new(PyModule {
        name: "pyexpat".to_owned(),
        filename: None,
        dict,
    })
}
