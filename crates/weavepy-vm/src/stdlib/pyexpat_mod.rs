//! `pyexpat` — native XML parser (RFC 0040 WS5).
//!
//! CPython's `pyexpat` is a thin C wrapper over the bundled `expat`
//! library. WeavePy wraps `quick-xml` (a mature pure-Rust streaming XML
//! parser) behind the same Python-visible surface: `ParserCreate`, the
//! `xmlparser` object with its settable `*Handler` callbacks, `Parse` /
//! `ParseFile`, the `Error*`/`Current*` position attributes, and the
//! `ExpatError` exception with `code`/`lineno`/`offset`.
//!
//! This is what makes `xml.parsers.expat`, `xml.sax`, `xml.dom.minidom`
//! and — critically for WS5 — the `xmlrpc.client` serializer that
//! `multiprocessing.managers` uses (`serializer='xmlrpclib'`) work. The
//! manager-server restart tests (`test_rapid_restart`, `test_remote`)
//! cannot complete without it, and their orphaned server children are
//! what deadlock the spawn suite during cleanup.
//!
//! ## Push model
//!
//! `expat` is a push parser: `Parse(data, isfinal)` is fed incrementally
//! and fires handlers as tokens complete. `quick-xml` is a pull parser,
//! so we accumulate the fed bytes and run the parse when the document is
//! finalised (`isfinal=True`). Every real consumer here feeds a complete
//! document and then closes (`xmlrpc`'s `loads`, `sax.parseString`,
//! `minidom.parseString`), so the handler *sequence* and final result are
//! faithful; only the intra-`feed` interleaving differs.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

// ---------------------------------------------------------------------------
// Parser state registry (mirrors the `_bz2` streaming-object pattern: native
// state lives in a process-global registry keyed by an integer handle stored
// on the Python instance's `_handle`).
// ---------------------------------------------------------------------------

struct ExpatState {
    /// Bytes accumulated across `Parse(data, isfinal=False)` calls.
    buffer: Vec<u8>,
    /// Namespace separator (expat's `namespace_separator`); `None` disables
    /// namespace processing.
    namespace_sep: Option<String>,
    /// Coalesce adjacent character data into a single handler call.
    buffer_text: bool,
    /// Report attributes as a flat `[n0, v0, n1, v1, …]` list instead of a
    /// dict.
    ordered_attributes: bool,
    /// Already-finalised (a second non-empty `Parse` after `isfinal=True`
    /// is an error in expat).
    finished: bool,
    /// Cached newline offsets of `buffer`, for line/column reporting.
    line_starts: Vec<usize>,
}

// All fields are `Send`; the registry `Mutex` provides the cross-thread
// barrier (a parser object can be created on one thread and used on another,
// as the manager-server feeder does).

type ParserReg = Mutex<HashMap<i64, Rc<RefCell<ExpatState>>>>;

fn parser_reg() -> &'static ParserReg {
    static REG: OnceLock<ParserReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn state_of(id: i64) -> Option<Rc<RefCell<ExpatState>>> {
    parser_reg().lock().ok()?.get(&id).cloned()
}

// ---------------------------------------------------------------------------
// Module construction.
// ---------------------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("pyexpat"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Python wrapper for Expat parser (RFC 0040 WS5)."),
        );
        d.insert(
            DictKey(Object::from_static("EXPAT_VERSION")),
            Object::from_static("expat_2.5.0"),
        );
        d.insert(
            DictKey(Object::from_static("version_info")),
            Object::new_tuple(vec![Object::Int(2), Object::Int(5), Object::Int(0)]),
        );
        d.insert(
            DictKey(Object::from_static("native_encoding")),
            Object::from_static("UTF-8"),
        );
        d.insert(
            DictKey(Object::from_static("features")),
            Object::new_tuple(vec![]),
        );
        d.insert(
            DictKey(Object::from_static("XML_PARAM_ENTITY_PARSING_NEVER")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static(
                "XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE",
            )),
            Object::Int(1),
        );
        d.insert(
            DictKey(Object::from_static("XML_PARAM_ENTITY_PARSING_ALWAYS")),
            Object::Int(2),
        );
        d.insert(
            DictKey(Object::from_static("ParserCreate")),
            b_kw("ParserCreate", parser_create),
        );
        d.insert(
            DictKey(Object::from_static("ErrorString")),
            b("ErrorString", error_string),
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

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn b_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

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

// ---------------------------------------------------------------------------
// ExpatError exception + the `errors` / `model` submodules.
// ---------------------------------------------------------------------------

fn expat_error_type() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let parent = crate::builtin_types::builtin_types().exception.clone();
        let cls = TypeObject::new_exception("ExpatError", parent).expect("ExpatError class");
        crate::stdlib::io::set_type_module(&cls, "xml.parsers.expat");
        cls
    })
    .clone()
}

/// expat error codes (subset; the well-formedness ones the tests check).
const ERR_CODES: &[(i64, &str, &str)] = &[
    (1, "XML_ERROR_NO_MEMORY", "out of memory"),
    (2, "XML_ERROR_SYNTAX", "syntax error"),
    (3, "XML_ERROR_NO_ELEMENTS", "no element found"),
    (
        4,
        "XML_ERROR_INVALID_TOKEN",
        "not well-formed (invalid token)",
    ),
    (5, "XML_ERROR_UNCLOSED_TOKEN", "unclosed token"),
    (6, "XML_ERROR_PARTIAL_CHAR", "partial character"),
    (7, "XML_ERROR_TAG_MISMATCH", "mismatched tag"),
    (8, "XML_ERROR_DUPLICATE_ATTRIBUTE", "duplicate attribute"),
    (
        9,
        "XML_ERROR_JUNK_AFTER_DOC_ELEMENT",
        "junk after document element",
    ),
    (
        10,
        "XML_ERROR_PARAM_ENTITY_REF",
        "illegal parameter entity reference",
    ),
    (11, "XML_ERROR_UNDEFINED_ENTITY", "undefined entity"),
    (
        12,
        "XML_ERROR_RECURSIVE_ENTITY_REF",
        "recursive entity reference",
    ),
    (13, "XML_ERROR_ASYNC_ENTITY", "asynchronous entity"),
    (
        14,
        "XML_ERROR_BAD_CHAR_REF",
        "reference to invalid character number",
    ),
    (
        15,
        "XML_ERROR_BINARY_ENTITY_REF",
        "reference to binary entity",
    ),
    (
        16,
        "XML_ERROR_ATTRIBUTE_EXTERNAL_ENTITY_REF",
        "reference to external entity in attribute",
    ),
    (
        17,
        "XML_ERROR_MISPLACED_XML_PI",
        "XML or text declaration not at start of entity",
    ),
    (18, "XML_ERROR_UNKNOWN_ENCODING", "unknown encoding"),
    (
        19,
        "XML_ERROR_INCORRECT_ENCODING",
        "encoding specified in XML declaration is incorrect",
    ),
    (
        20,
        "XML_ERROR_UNCLOSED_CDATA_SECTION",
        "unclosed CDATA section",
    ),
    (
        21,
        "XML_ERROR_EXTERNAL_ENTITY_HANDLING",
        "error in processing external entity reference",
    ),
    (22, "XML_ERROR_NOT_STANDALONE", "document is not standalone"),
];

fn error_message(code: i64) -> &'static str {
    ERR_CODES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, _, m)| *m)
        .unwrap_or("unknown error")
}

fn error_string(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = args.first().and_then(Object::as_i64).unwrap_or(0);
    Ok(Object::from_str(error_message(code).to_owned()))
}

fn errors_submodule() -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("xml.parsers.expat.errors"),
        );
        // `codes`: message -> code; `messages`: code -> message.
        let codes = Rc::new(RefCell::new(DictData::new()));
        let messages = Rc::new(RefCell::new(DictData::new()));
        {
            let mut c = codes.borrow_mut();
            let mut m = messages.borrow_mut();
            for (code, name, msg) in ERR_CODES {
                d.insert(
                    DictKey(Object::from_static(name)),
                    Object::from_str((*msg).to_owned()),
                );
                c.insert(
                    DictKey(Object::from_str((*msg).to_owned())),
                    Object::Int(*code),
                );
                m.insert(
                    DictKey(Object::Int(*code)),
                    Object::from_str((*msg).to_owned()),
                );
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
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("xml.parsers.expat.model"),
        );
        for (name, val) in [
            ("XML_CTYPE_EMPTY", 1),
            ("XML_CTYPE_ANY", 2),
            ("XML_CTYPE_MIXED", 3),
            ("XML_CTYPE_NAME", 4),
            ("XML_CTYPE_CHOICE", 5),
            ("XML_CTYPE_SEQ", 6),
            ("XML_CQUANT_NONE", 0),
            ("XML_CQUANT_OPT", 1),
            ("XML_CQUANT_REP", 2),
            ("XML_CQUANT_PLUS", 3),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
    }
    Rc::new(PyModule {
        name: "xml.parsers.expat.model".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// xmlparser type + ParserCreate.
// ---------------------------------------------------------------------------

fn parser_type() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let mut d = DictData::new();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("pyexpat"),
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
        TypeObject::new_with_flags(
            "xmlparser",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("xmlparser type")
    })
    .clone()
}

fn opt_str(o: Option<&Object>) -> Option<String> {
    match o {
        Some(Object::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn parser_create(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mut encoding = opt_str(args.first());
    let mut namespace_sep = opt_str(args.get(1));
    let mut intern = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "encoding" => encoding = opt_str(Some(v)),
            "namespace_separator" => namespace_sep = opt_str(Some(v)),
            "intern" => intern = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "ParserCreate() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // expat requires the namespace separator to be a single character.
    if let Some(s) = &namespace_sep {
        if s.chars().count() > 1 {
            return Err(value_error(
                "namespace_separator must be at most one character, omitted, or None",
            ));
        }
    }
    let parser = make_parser(encoding, namespace_sep);
    // An explicit `intern` mapping (or `None`) overrides the default dict.
    if let (Some(intern), Object::Instance(inst)) = (intern, &parser) {
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("intern")), intern);
    }
    Ok(parser)
}

fn make_parser(encoding: Option<String>, namespace_sep: Option<String>) -> Object {
    let id = next_id();
    if let Ok(mut reg) = parser_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(ExpatState {
                buffer: Vec::new(),
                namespace_sep,
                buffer_text: false,
                ordered_attributes: false,
                finished: false,
                line_starts: vec![0],
            })),
        );
    }
    let inst = PyInstance::new(parser_type());
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
        d.insert(
            DictKey(Object::from_static("buffer_text")),
            Object::Bool(false),
        );
        d.insert(
            DictKey(Object::from_static("ordered_attributes")),
            Object::Bool(false),
        );
        d.insert(
            DictKey(Object::from_static("specified_attributes")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("namespace_prefixes")),
            Object::Bool(false),
        );
        d.insert(
            DictKey(Object::from_static("buffer_size")),
            Object::Int(8192),
        );
        d.insert(DictKey(Object::from_static("buffer_used")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("CurrentLineNumber")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("CurrentColumnNumber")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("CurrentByteIndex")),
            Object::Int(0),
        );
        d.insert(DictKey(Object::from_static("ErrorCode")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("ErrorLineNumber")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("ErrorColumnNumber")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("ErrorByteIndex")),
            Object::Int(0),
        );
        let enc = match encoding {
            Some(e) => Object::from_str(e),
            None => Object::None,
        };
        d.insert(DictKey(Object::from_static("encoding")), enc);
        d.insert(DictKey(Object::from_static("base")), Object::None);
        // CPython's `xmlparser.intern` is the string-interning dict (the
        // `intern` ctor argument; defaults to a fresh dict). `minidom`'s
        // expat builder calls `self._parser.intern.setdefault`.
        d.insert(
            DictKey(Object::from_static("intern")),
            Object::Dict(Rc::new(RefCell::new(DictData::new()))),
        );
    }
    Object::Instance(Rc::new(inst))
}

fn self_inst(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("expected xmlparser instance")),
    }
}

fn handle_of(inst: &Rc<PyInstance>) -> Result<i64, RuntimeError> {
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
    {
        Some(Object::Int(v)) => Ok(v),
        _ => Err(type_error("xmlparser missing _handle")),
    }
}

fn flag_attr(inst: &Rc<PyInstance>, name: &'static str) -> bool {
    matches!(
        inst.dict
            .borrow()
            .get(&DictKey(Object::from_static(name)))
            .cloned(),
        Some(Object::Bool(true)) | Some(Object::Int(1))
    )
}

// ---------------------------------------------------------------------------
// Handler dispatch.
// ---------------------------------------------------------------------------

fn handler_of(inst: &Rc<PyInstance>, name: &'static str) -> Option<Object> {
    let d = inst.dict.borrow();
    match d.get(&DictKey(Object::from_static(name))) {
        None | Some(Object::None) => None,
        Some(o) => Some(o.clone()),
    }
}

fn call_handler(
    inst: &Rc<PyInstance>,
    name: &'static str,
    args: &[Object],
) -> Result<bool, RuntimeError> {
    let Some(h) = handler_of(inst, name) else {
        return Ok(false);
    };
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for expat handler"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    interp.call_object(h, args, &[])?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Parse.
// ---------------------------------------------------------------------------

fn parse_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let id = handle_of(&inst)?;
    let state = state_of(id).ok_or_else(|| value_error("stale xmlparser"))?;

    let (data, isfinal) = parse_args(&args[1..])?;

    {
        let mut st = state.borrow_mut();
        if st.finished {
            return Err(make_expat_error(
                &inst,
                9,
                st.line_starts.len() as i64,
                0,
                st.buffer.len() as i64,
            ));
        }
        st.buffer.extend_from_slice(&data);
        if !isfinal {
            // Defer: accumulate until the document is finalised.
            return Ok(Object::Int(1));
        }
        st.finished = true;
    }

    // Snapshot the buffer + flags, then release the registry lock so handler
    // callbacks (which may touch the parser) don't deadlock.
    let (buffer, namespace_sep, buffer_text, ordered) = {
        let st = state.borrow();
        (
            st.buffer.clone(),
            st.namespace_sep.clone(),
            st.buffer_text,
            st.ordered_attributes,
        )
    };
    // The `buffer_text` / `ordered_attributes` knobs are user-settable on the
    // instance after creation; honour the instance attributes.
    let buffer_text = buffer_text || flag_attr(&inst, "buffer_text");
    let ordered = ordered || flag_attr(&inst, "ordered_attributes");

    let line_starts = compute_line_starts(&buffer);
    {
        let mut st = state.borrow_mut();
        st.line_starts = line_starts.clone();
    }

    run_parse(
        &inst,
        &buffer,
        &line_starts,
        namespace_sep.as_deref(),
        buffer_text,
        ordered,
    )?;
    Ok(Object::Int(1))
}

fn parse_file_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    // `ParseFile(file)` — read the whole file, then parse as a final chunk.
    let file = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("ParseFile() takes a file argument"))?;
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter"))?;
    let interp = unsafe { &mut *ptr };
    let read = interp.load_attr_public(&file, "read")?;
    let data = interp.call_object(read, &[], &[])?;
    let bytes = match &data {
        Object::Bytes(b) => b.to_vec(),
        Object::ByteArray(b) => b.borrow().clone(),
        Object::Str(s) => s.as_bytes().to_vec(),
        _ => return Err(type_error("read() must return bytes")),
    };
    parse_method(&[
        Object::Instance(inst),
        Object::new_bytes(bytes),
        Object::Bool(true),
    ])
}

fn parse_args(rest: &[Object]) -> Result<(Vec<u8>, bool), RuntimeError> {
    let data = match rest.first() {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        None => Vec::new(),
        _ => return Err(type_error("Parse() argument must be str or bytes")),
    };
    let isfinal = match rest.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        _ => false,
    };
    Ok((data, isfinal))
}

fn compute_line_starts(buf: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Line (0-based) and column (0-based) for `offset` — expat's
/// `CurrentLineNumber` is 1-based, `CurrentColumnNumber` 0-based.
fn line_col(line_starts: &[usize], offset: usize) -> (i64, i64) {
    // Largest index whose start <= offset.
    let idx = match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let line = idx as i64 + 1;
    let col = offset.saturating_sub(line_starts[idx]) as i64;
    (line, col)
}

fn update_position(inst: &Rc<PyInstance>, line_starts: &[usize], offset: usize) {
    let (line, col) = line_col(line_starts, offset);
    let mut d = inst.dict.borrow_mut();
    d.insert(
        DictKey(Object::from_static("CurrentLineNumber")),
        Object::Int(line),
    );
    d.insert(
        DictKey(Object::from_static("CurrentColumnNumber")),
        Object::Int(col),
    );
    d.insert(
        DictKey(Object::from_static("CurrentByteIndex")),
        Object::Int(offset as i64),
    );
}

fn make_expat_error(
    inst: &Rc<PyInstance>,
    code: i64,
    lineno: i64,
    colno: i64,
    byte_index: i64,
) -> RuntimeError {
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("ErrorCode")), Object::Int(code));
        d.insert(
            DictKey(Object::from_static("ErrorLineNumber")),
            Object::Int(lineno),
        );
        d.insert(
            DictKey(Object::from_static("ErrorColumnNumber")),
            Object::Int(colno),
        );
        d.insert(
            DictKey(Object::from_static("ErrorByteIndex")),
            Object::Int(byte_index),
        );
    }
    let msg = format!("{}: line {}, column {}", error_message(code), lineno, colno);
    let cls = expat_error_type();
    let einst = PyInstance::new(cls);
    {
        let mut d = einst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![Object::from_str(msg.clone())]),
        );
        d.insert(DictKey(Object::from_static("code")), Object::Int(code));
        d.insert(DictKey(Object::from_static("lineno")), Object::Int(lineno));
        d.insert(DictKey(Object::from_static("offset")), Object::Int(colno));
    }
    RuntimeError::PyException(PyException::new(Object::Instance(Rc::new(einst))))
}

/// One namespace scope: prefix → URI. The empty prefix is the default
/// namespace (`xmlns="…"`).
type NsScope = HashMap<String, String>;

fn run_parse(
    inst: &Rc<PyInstance>,
    buffer: &[u8],
    line_starts: &[usize],
    namespace_sep: Option<&str>,
    buffer_text: bool,
    ordered: bool,
) -> Result<(), RuntimeError> {
    let mut reader = Reader::from_reader(buffer);
    let config = reader.config_mut();
    config.trim_text(false);
    config.expand_empty_elements = false;
    config.check_end_names = true;

    let mut ns_stack: Vec<NsScope> = Vec::new();
    let mut pending_text: Option<String> = None;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let event_pos = reader.buffer_position() as usize;
        let ev = reader.read_event_into(&mut buf);
        match ev {
            Ok(Event::Eof) => break,
            Ok(Event::Decl(e)) => {
                flush_text(inst, &mut pending_text)?;
                let version = e
                    .version()
                    .ok()
                    .map(|v| decode_cow(&reader, &v))
                    .transpose()?
                    .unwrap_or_default();
                let encoding = match e.encoding() {
                    Some(Ok(enc)) => Some(decode_cow(&reader, &enc)?),
                    _ => None,
                };
                let standalone = match e.standalone() {
                    Some(Ok(sa)) => {
                        let s = decode_cow(&reader, &sa)?;
                        if s == "yes" {
                            Object::Int(1)
                        } else if s == "no" {
                            Object::Int(0)
                        } else {
                            Object::Int(-1)
                        }
                    }
                    _ => Object::Int(-1),
                };
                update_position(inst, line_starts, event_pos);
                let enc_obj = match encoding {
                    Some(e) => Object::from_str(e),
                    None => Object::None,
                };
                call_handler(
                    inst,
                    "XmlDeclHandler",
                    &[Object::from_str(version), enc_obj, standalone],
                )?;
            }
            Ok(Event::Start(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                handle_start(
                    inst,
                    &reader,
                    &e,
                    namespace_sep,
                    ordered,
                    &mut ns_stack,
                    false,
                )?;
            }
            Ok(Event::Empty(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                handle_start(
                    inst,
                    &reader,
                    &e,
                    namespace_sep,
                    ordered,
                    &mut ns_stack,
                    true,
                )?;
            }
            Ok(Event::End(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                let name = expand_end_name(&reader, e.name().as_ref(), namespace_sep, &ns_stack)?;
                call_handler(inst, "EndElementHandler", &[Object::from_str(name)])?;
                pop_ns_scope(inst, namespace_sep, &mut ns_stack)?;
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| escape_err(inst, line_starts, event_pos, &err.to_string()))?
                    .into_owned();
                accumulate_text(inst, &mut pending_text, text, buffer_text)?;
            }
            Ok(Event::CData(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                let text = decode_cow(&reader, e.as_ref())?;
                call_handler(inst, "StartCdataSectionHandler", &[])?;
                emit_char_data(inst, &text)?;
                call_handler(inst, "EndCdataSectionHandler", &[])?;
            }
            Ok(Event::Comment(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                let text = decode_cow(&reader, e.as_ref())?;
                call_handler(inst, "CommentHandler", &[Object::from_str(text)])?;
            }
            Ok(Event::PI(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                let raw = decode_cow(&reader, e.as_ref())?;
                let (target, rest) = match raw.split_once(char::is_whitespace) {
                    Some((t, r)) => (t.to_owned(), r.trim_start().to_owned()),
                    None => (raw.clone(), String::new()),
                };
                call_handler(
                    inst,
                    "ProcessingInstructionHandler",
                    &[Object::from_str(target), Object::from_str(rest)],
                )?;
            }
            Ok(Event::DocType(e)) => {
                flush_text(inst, &mut pending_text)?;
                update_position(inst, line_starts, event_pos);
                let text = decode_cow(&reader, e.as_ref())?;
                let name = text.split_whitespace().next().unwrap_or("").to_owned();
                call_handler(
                    inst,
                    "StartDoctypeDeclHandler",
                    &[
                        Object::from_str(name),
                        Object::None,
                        Object::None,
                        Object::Int(0),
                    ],
                )?;
                call_handler(inst, "EndDoctypeDeclHandler", &[])?;
            }
            #[allow(unreachable_patterns)]
            Ok(_) => {}
            Err(err) => {
                let (line, col) = line_col(line_starts, event_pos);
                return Err(map_quickxml_error(inst, &err, line, col, event_pos as i64));
            }
        }
        buf.clear();
    }
    flush_text(inst, &mut pending_text)?;
    Ok(())
}

fn accumulate_text(
    inst: &Rc<PyInstance>,
    pending: &mut Option<String>,
    text: String,
    buffer_text: bool,
) -> Result<(), RuntimeError> {
    if buffer_text {
        pending.get_or_insert_with(String::new).push_str(&text);
        Ok(())
    } else {
        emit_char_data(inst, &text)
    }
}

fn flush_text(inst: &Rc<PyInstance>, pending: &mut Option<String>) -> Result<(), RuntimeError> {
    if let Some(text) = pending.take() {
        if !text.is_empty() {
            emit_char_data(inst, &text)?;
        }
    }
    Ok(())
}

fn emit_char_data(inst: &Rc<PyInstance>, text: &str) -> Result<(), RuntimeError> {
    let fired = call_handler(
        inst,
        "CharacterDataHandler",
        &[Object::from_str(text.to_owned())],
    )?;
    if !fired {
        // expat routes unhandled data through the Default handlers.
        if handler_of(inst, "DefaultHandlerExpand").is_some() {
            call_handler(
                inst,
                "DefaultHandlerExpand",
                &[Object::from_str(text.to_owned())],
            )?;
        } else {
            call_handler(inst, "DefaultHandler", &[Object::from_str(text.to_owned())])?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_start(
    inst: &Rc<PyInstance>,
    reader: &Reader<&[u8]>,
    e: &quick_xml::events::BytesStart<'_>,
    namespace_sep: Option<&str>,
    ordered: bool,
    ns_stack: &mut Vec<NsScope>,
    empty: bool,
) -> Result<(), RuntimeError> {
    // Collect attributes, separating xmlns declarations when namespace
    // processing is on.
    let mut scope: NsScope = HashMap::new();
    let mut raw_attrs: Vec<(String, String)> = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|err| value_error(format!("expat: bad attribute: {err}")))?;
        let key = decode_cow(reader, a.key.as_ref())?;
        let val = decode_attr_value(reader, &a.value)?;
        if namespace_sep.is_some() && (key == "xmlns" || key.starts_with("xmlns:")) {
            let prefix = if key == "xmlns" {
                String::new()
            } else {
                key["xmlns:".len()..].to_owned()
            };
            scope.insert(prefix.clone(), val.clone());
            // expat reports namespace declarations before the start tag.
            let prefix_obj = if prefix.is_empty() {
                Object::None
            } else {
                Object::from_str(prefix.clone())
            };
            call_handler(
                inst,
                "StartNamespaceDeclHandler",
                &[prefix_obj, Object::from_str(val)],
            )?;
        } else {
            raw_attrs.push((key, val));
        }
    }
    if namespace_sep.is_some() {
        ns_stack.push(scope);
    }

    let name = expand_name(reader, e.name().as_ref(), namespace_sep, ns_stack, true)?;

    // Build the expanded attribute names.
    let mut attrs: Vec<(String, String)> = Vec::with_capacity(raw_attrs.len());
    for (k, v) in raw_attrs {
        let nk = if namespace_sep.is_some() {
            expand_name(reader, k.as_bytes(), namespace_sep, ns_stack, false)?
        } else {
            k
        };
        attrs.push((nk, v));
    }

    let attr_obj = if ordered {
        let mut items = Vec::with_capacity(attrs.len() * 2);
        for (k, v) in attrs {
            items.push(Object::from_str(k));
            items.push(Object::from_str(v));
        }
        Object::List(Rc::new(RefCell::new(items)))
    } else {
        let dict = Rc::new(RefCell::new(DictData::new()));
        {
            let mut d = dict.borrow_mut();
            for (k, v) in attrs {
                d.insert(DictKey(Object::from_str(k)), Object::from_str(v));
            }
        }
        Object::Dict(dict)
    };

    call_handler(
        inst,
        "StartElementHandler",
        &[Object::from_str(name.clone()), attr_obj],
    )?;

    if empty {
        call_handler(inst, "EndElementHandler", &[Object::from_str(name)])?;
        if namespace_sep.is_some() {
            pop_ns_scope(inst, namespace_sep, ns_stack)?;
        }
    }
    Ok(())
}

fn pop_ns_scope(
    inst: &Rc<PyInstance>,
    namespace_sep: Option<&str>,
    ns_stack: &mut Vec<NsScope>,
) -> Result<(), RuntimeError> {
    if namespace_sep.is_none() {
        return Ok(());
    }
    if let Some(scope) = ns_stack.pop() {
        for prefix in scope.keys() {
            let prefix_obj = if prefix.is_empty() {
                Object::None
            } else {
                Object::from_str(prefix.clone())
            };
            call_handler(inst, "EndNamespaceDeclHandler", &[prefix_obj])?;
        }
    }
    Ok(())
}

fn lookup_ns(ns_stack: &[NsScope], prefix: &str) -> Option<String> {
    for scope in ns_stack.iter().rev() {
        if let Some(uri) = scope.get(prefix) {
            return Some(uri.clone());
        }
    }
    None
}

/// Expand an element/attribute name in namespace mode to expat's
/// `uri<sep>localname` form. `is_element` controls default-namespace
/// application (attributes are not in the default namespace).
fn expand_name(
    reader: &Reader<&[u8]>,
    raw: &[u8],
    namespace_sep: Option<&str>,
    ns_stack: &[NsScope],
    is_element: bool,
) -> Result<String, RuntimeError> {
    let name = decode_cow(reader, raw)?;
    let Some(sep) = namespace_sep else {
        return Ok(name);
    };
    if let Some((prefix, local)) = name.split_once(':') {
        if let Some(uri) = lookup_ns(ns_stack, prefix) {
            return Ok(format!("{uri}{sep}{local}"));
        }
        return Ok(name);
    }
    if is_element {
        if let Some(uri) = lookup_ns(ns_stack, "") {
            if !uri.is_empty() {
                return Ok(format!("{uri}{sep}{name}"));
            }
        }
    }
    Ok(name)
}

fn expand_end_name(
    reader: &Reader<&[u8]>,
    raw: &[u8],
    namespace_sep: Option<&str>,
    ns_stack: &[NsScope],
) -> Result<String, RuntimeError> {
    expand_name(reader, raw, namespace_sep, ns_stack, true)
}

fn decode_cow(reader: &Reader<&[u8]>, bytes: &[u8]) -> Result<String, RuntimeError> {
    reader
        .decoder()
        .decode(bytes)
        .map(|c| c.into_owned())
        .map_err(|e| value_error(format!("expat: decode error: {e}")))
}

fn decode_attr_value(reader: &Reader<&[u8]>, bytes: &[u8]) -> Result<String, RuntimeError> {
    let decoded = decode_cow(reader, bytes)?;
    quick_xml::escape::unescape(&decoded)
        .map(|c| c.into_owned())
        .map_err(|e| value_error(format!("expat: unescape error: {e}")))
}

fn escape_err(
    inst: &Rc<PyInstance>,
    line_starts: &[usize],
    offset: usize,
    _msg: &str,
) -> RuntimeError {
    let (line, col) = line_col(line_starts, offset);
    make_expat_error(inst, 4, line, col, offset as i64)
}

fn map_quickxml_error(
    inst: &Rc<PyInstance>,
    err: &quick_xml::Error,
    line: i64,
    col: i64,
    byte_index: i64,
) -> RuntimeError {
    use quick_xml::Error as E;
    let code = match err {
        E::IllFormed(_) => 7,
        E::Syntax(_) => 2,
        _ => 4,
    };
    make_expat_error(inst, code, line, col, byte_index)
}

// ---------------------------------------------------------------------------
// Misc methods.
// ---------------------------------------------------------------------------

fn set_base_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let base = args.get(1).cloned().unwrap_or(Object::None);
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("base")), base);
    Ok(Object::None)
}

fn get_base_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let base = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("base")))
        .cloned()
        .unwrap_or(Object::None);
    Ok(base)
}

fn get_input_context_method(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn set_param_entity_parsing_method(_args: &[Object]) -> Result<Object, RuntimeError> {
    // We never read external/parameter entities; report "not changed".
    Ok(Object::Int(0))
}

fn use_foreign_dtd_method(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn get_reparse_deferral_method(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn set_reparse_deferral_method(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn external_entity_parser_create(args: &[Object]) -> Result<Object, RuntimeError> {
    // Return a fresh parser sharing the namespace configuration.
    let inst = self_inst(args)?;
    let ns = {
        let id = handle_of(&inst)?;
        state_of(id).and_then(|s| s.borrow().namespace_sep.clone())
    };
    Ok(make_parser(None, ns))
}
