//! The `_csv` accelerator module — a faithful port of CPython's
//! `Modules/_csv.c`.
//!
//! WeavePy ships a verbatim pure-Python `csv` (`stdlib/python/csv.py`)
//! that does `from _csv import Error, writer, reader, register_dialect,
//! unregister_dialect, get_dialect, list_dialects, field_size_limit,
//! QUOTE_*` and `from _csv import Dialect as _Dialect`. So the
//! accelerator must provide all of those names with CPython-faithful
//! semantics: the DFA-based reader (`Reader_iternext`), the quoting/
//! escaping writer (`join_append*`), the validated `Dialect` type with
//! read-only attributes, the dialect registry, and `field_size_limit`.
//!
//! Behaviour is matched against `Modules/_csv.c` (the reader state
//! machine, the six quote styles, the tri-state attribute resolution —
//! "not given" vs `None` vs value — and the exact `TypeError` /
//! `ValueError` / `_csv.Error` messages `test_csv.py` asserts on).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtin_types::builtin_types;
use crate::error::{stop_iteration, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, PyProperty};
use crate::types::{PyInstance, TypeObject};
use crate::Interpreter;

// Quote styles (mirrors the `QuoteStyle` enum + module int constants).
const QUOTE_MINIMAL: i64 = 0;
const QUOTE_ALL: i64 = 1;
const QUOTE_NONNUMERIC: i64 = 2;
const QUOTE_NONE: i64 = 3;
const QUOTE_STRINGS: i64 = 4;
const QUOTE_NOTNULL: i64 = 5;

/// `module_state->field_limit`. CPython defaults it to `128 * 1024` and
/// stores it in per-module state; a process-global atomic is equivalent
/// for the single-interpreter conformance run and keeps the reader's
/// `__next__` (a method on the shared type) from needing to capture it.
static FIELD_LIMIT: AtomicI64 = AtomicI64::new(128 * 1024);

// ---------------------------------------------------------------------------
// Module build
// ---------------------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(key("__name__"), Object::from_static("_csv"));
        d.insert(
            key("__doc__"),
            Object::from_static("CSV parsing and writing.\n"),
        );
        d.insert(key("__version__"), Object::from_static("1.0"));
        d.insert(key("QUOTE_MINIMAL"), Object::Int(QUOTE_MINIMAL));
        d.insert(key("QUOTE_ALL"), Object::Int(QUOTE_ALL));
        d.insert(key("QUOTE_NONNUMERIC"), Object::Int(QUOTE_NONNUMERIC));
        d.insert(key("QUOTE_NONE"), Object::Int(QUOTE_NONE));
        d.insert(key("QUOTE_STRINGS"), Object::Int(QUOTE_STRINGS));
        d.insert(key("QUOTE_NOTNULL"), Object::Int(QUOTE_NOTNULL));
        d.insert(key("Error"), Object::Type(error_class()));
        d.insert(key("Dialect"), Object::Type(dialect_type()));
        d.insert(key("Reader"), Object::Type(reader_type()));
        d.insert(key("Writer"), Object::Type(writer_type()));
        d.insert(key("_dialects"), Object::Dict(dialects().clone()));
        d.insert(key("reader"), bkw("reader", csv_reader));
        d.insert(key("writer"), bkw("writer", csv_writer));
        d.insert(
            key("register_dialect"),
            bkw("register_dialect", csv_register_dialect),
        );
        d.insert(
            key("unregister_dialect"),
            b("unregister_dialect", csv_unregister_dialect),
        );
        d.insert(key("get_dialect"), b("get_dialect", csv_get_dialect));
        d.insert(key("list_dialects"), b("list_dialects", csv_list_dialects));
        d.insert(
            key("field_size_limit"),
            b("field_size_limit", csv_field_size_limit),
        );
    }
    Rc::new(PyModule {
        name: "_csv".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// Small construction helpers
// ---------------------------------------------------------------------------

fn key(s: &'static str) -> DictKey {
    DictKey(Object::from_static(s))
}

/// The reader's own state keys, which every row reads and writes: built
/// once, where [`key`] allocates a string per use.
struct ReaderKeys {
    input_iter: DictKey,
    cfg: DictKey,
    line_num: DictKey,
    fields_live: DictKey,
}

fn reader_keys() -> &'static ReaderKeys {
    static KEYS: OnceLock<ReaderKeys> = OnceLock::new();
    KEYS.get_or_init(|| ReaderKeys {
        input_iter: key("input_iter"),
        cfg: key("_cfg"),
        line_num: key("line_num"),
        fields_live: key("_fields_live"),
    })
}

/// A dialect's attribute keys, which every reader's dialect is built with
/// and read by (and a reader's `dialect`): built once.
struct DialectKeys {
    delimiter: DictKey,
    quotechar: DictKey,
    escapechar: DictKey,
    lineterminator: DictKey,
    quoting: DictKey,
    doublequote: DictKey,
    skipinitialspace: DictKey,
    strict: DictKey,
    dialect: DictKey,
}

fn dialect_keys() -> &'static DialectKeys {
    static KEYS: OnceLock<DialectKeys> = OnceLock::new();
    KEYS.get_or_init(|| DialectKeys {
        delimiter: key("delimiter"),
        quotechar: key("quotechar"),
        escapechar: key("escapechar"),
        lineterminator: key("lineterminator"),
        quoting: key("quoting"),
        doublequote: key("doublequote"),
        skipinitialspace: key("skipinitialspace"),
        strict: key("strict"),
        dialect: key("dialect"),
    })
}

/// A module-level function taking only positional args.
fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// A module-level function that also accepts keyword arguments.
fn bkw(
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

/// An instance method (binds `self`).
fn method(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Borrow the active interpreter published on this thread by the dispatch
/// loop. Always present while a builtin runs.
fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_csv: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

/// Build a `_csv.Error` exception carrying `msg` as its single arg, so
/// `str(err)` renders the message (matching CPython's `PyErr_Format`).
fn csv_error(msg: impl Into<String>) -> RuntimeError {
    let inst = crate::builtin_types::make_exception_with_class(error_class(), msg);
    RuntimeError::PyException(PyException::new(inst))
}

// ---------------------------------------------------------------------------
// Process-global module state (shared types + registry)
// ---------------------------------------------------------------------------

/// `module_state->error_obj` — the `_csv.Error` exception class. A
/// process-global singleton so `isinstance(err, csv.Error)` and
/// `class Foo(csv.Error)` see one stable identity.
fn error_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let parent = builtin_types().exception.clone();
        let cls = TypeObject::new_exception("Error", parent).expect("_csv.Error must build");
        // `_csv.Error.__module__ == "_csv"` (CPython) — `test___all__`
        // treats it as a public re-export of `csv` only when its module is
        // `csv` or `_csv`.
        cls.dict
            .borrow_mut()
            .insert(key("__module__"), Object::from_static("_csv"));
        cls
    })
    .clone()
}

/// `module_state->dialects` — the name→Dialect registry, also exposed as
/// `_csv._dialects`.
fn dialects() -> &'static Rc<RefCell<DictData>> {
    static REG: OnceLock<Rc<RefCell<DictData>>> = OnceLock::new();
    REG.get_or_init(|| Rc::new(RefCell::new(DictData::default())))
}

fn is_dialect_instance(o: &Object) -> bool {
    matches!(o, Object::Instance(i) if i.cls().is_subclass_of(&dialect_type()))
}

// ---------------------------------------------------------------------------
// Dialect type
// ---------------------------------------------------------------------------

fn dialect_type() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = builtin_types();
        let mut dict = DictData::default();
        // `tp_new` does all validation and can return an existing dialect
        // (the reuse path). Registered as a plain builtin so the VM's
        // instantiate path treats it as a user `__new__` and uses its
        // return value directly.
        dict.insert(
            key("__new__"),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(|args| dialect_new(args, &[])),
                call_kw: Some(Box::new(dialect_new)),
            })),
        );
        // Dialects are unpicklable (CPython overrides __reduce__ to raise).
        dict.insert(key("__reduce__"), method("__reduce__", dialect_reduce));
        dict.insert(
            key("__reduce_ex__"),
            method("__reduce_ex__", dialect_reduce),
        );
        let cls = TypeObject::new_user("_csv.Dialect", vec![bt.object_.clone()], dict)
            .expect("_csv.Dialect must linearise");
        for name in [
            "delimiter",
            "doublequote",
            "escapechar",
            "lineterminator",
            "quotechar",
            "quoting",
            "skipinitialspace",
            "strict",
        ] {
            install_ro_getset(&cls, name);
        }
        cls
    })
    .clone()
}

/// Install a read-only computed attribute. The getter returns the value
/// stashed on the instance under `name`; the absent setter/deleter makes
/// `setattr`/`delattr` raise `AttributeError` (CPython's read-only
/// members/getset).
fn install_ro_getset(cls: &Rc<TypeObject>, name: &'static str) {
    let getter = Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |args| {
            let inst = match args.first() {
                Some(Object::Instance(i)) => i,
                _ => return Err(type_error("descriptor requires a Dialect instance")),
            };
            let d = inst.dict_cell().borrow();
            Ok(d.get(&DictKey(Object::from_static(name)))
                .cloned()
                .unwrap_or(Object::None))
        }),
        call_kw: None,
    }));
    let prop = Object::Property(Rc::new(PyProperty::new(
        getter,
        Object::None,
        Object::None,
        Object::None,
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        cls.clone(),
        name,
        None,
    );
    cls.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), prop);
}

fn dialect_reduce(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot pickle '_csv.Dialect' instances"))
}

/// `dialect_new` — validate args/kwargs and construct (or reuse) a
/// `_csv.Dialect`. `args[0]` is the class (when invoked through the VM's
/// instantiate path); `args[1]` is the optional positional dialect.
fn dialect_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let dialect_pos = args.get(1).cloned();

    let mut p_dialect: Option<Object> = None;
    let mut delimiter: Option<Object> = None;
    let mut doublequote: Option<Object> = None;
    let mut escapechar: Option<Object> = None;
    let mut lineterminator: Option<Object> = None;
    let mut quotechar: Option<Object> = None;
    let mut quoting: Option<Object> = None;
    let mut skipinitialspace: Option<Object> = None;
    let mut strict: Option<Object> = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "dialect" => p_dialect = Some(v.clone()),
            "delimiter" => delimiter = Some(v.clone()),
            "doublequote" => doublequote = Some(v.clone()),
            "escapechar" => escapechar = Some(v.clone()),
            "lineterminator" => lineterminator = Some(v.clone()),
            "quotechar" => quotechar = Some(v.clone()),
            "quoting" => quoting = Some(v.clone()),
            "skipinitialspace" => skipinitialspace = Some(v.clone()),
            "strict" => strict = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "'{other}' is an invalid keyword argument for this function"
                )))
            }
        }
    }

    let mut dialect = match (dialect_pos, p_dialect) {
        (Some(_), Some(_)) => {
            return Err(type_error(
                "argument for Dialect given by name ('dialect') and position (1)",
            ))
        }
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    };

    with_interp(|interp| {
        // A string names a registered dialect.
        if let Some(Object::Str(name)) = &dialect {
            let found = dialects()
                .borrow()
                .get(&DictKey(Object::Str(name.clone())))
                .cloned();
            match found {
                Some(d) => dialect = Some(d),
                None => return Err(csv_error("unknown dialect")),
            }
        }

        // Reuse an existing Dialect verbatim when no overrides were given.
        if let Some(d) = &dialect {
            if is_dialect_instance(d)
                && delimiter.is_none()
                && doublequote.is_none()
                && escapechar.is_none()
                && lineterminator.is_none()
                && quotechar.is_none()
                && quoting.is_none()
                && skipinitialspace.is_none()
                && strict.is_none()
            {
                return Ok(d.clone());
            }
        }

        // Fill any unset field from the dialect object's attributes
        // (a missing attribute is silently ignored — `PyErr_Clear`).
        if let Some(src) = dialect.clone() {
            macro_rules! fill {
                ($var:ident, $name:literal) => {
                    if $var.is_none() {
                        match interp.load_attr(&src, $name) {
                            Ok(v) => $var = Some(v),
                            Err(RuntimeError::PyException(e))
                                if e.type_name() == "AttributeError" => {}
                            Err(e) => return Err(e),
                        }
                    }
                };
            }
            fill!(delimiter, "delimiter");
            fill!(doublequote, "doublequote");
            fill!(escapechar, "escapechar");
            fill!(lineterminator, "lineterminator");
            fill!(quotechar, "quotechar");
            fill!(quoting, "quoting");
            fill!(skipinitialspace, "skipinitialspace");
            fill!(strict, "strict");
        }

        // CPython's two tri-state-sensitive special cases need the raw
        // "was it given at all / given as None" state.
        let quotechar_is_none_src = matches!(quotechar, Some(Object::None));
        let quoting_was_unset = quoting.is_none();

        let delim = set_char("delimiter", delimiter.as_ref(), ',')?;
        let dq = set_bool(doublequote.as_ref(), true);
        let esc = set_char_or_none("escapechar", escapechar.as_ref(), None)?;
        let lineterm = set_str("lineterminator", lineterminator.as_ref(), "\r\n")?;
        let qc = set_char_or_none("quotechar", quotechar.as_ref(), Some('"'))?;
        let mut quot = set_int("quoting", quoting.as_ref(), QUOTE_MINIMAL)?;
        let sis = set_bool(skipinitialspace.as_ref(), false);
        let strict_v = set_bool(strict.as_ref(), false);

        check_quoting(quot)?;
        if quotechar_is_none_src && quoting_was_unset {
            quot = QUOTE_NONE;
        }
        if quot != QUOTE_NONE && qc.is_none() {
            return Err(type_error("quotechar must be set if quoting enabled"));
        }
        let lineterm = match lineterm {
            Some(s) => s,
            None => return Err(type_error("lineterminator must be set")),
        };
        check_char("delimiter", Some(delim), true, &lineterm)?;
        check_char("escapechar", esc, !sis, &lineterm)?;
        check_char("quotechar", qc, !sis, &lineterm)?;
        check_chars("delimiter", "escapechar", Some(delim), esc)?;
        check_chars("delimiter", "quotechar", Some(delim), qc)?;
        check_chars("escapechar", "quotechar", esc, qc)?;

        let inst = PyInstance::new(dialect_type());
        {
            let k = dialect_keys();
            let mut d = inst.dict_cell().borrow_mut();
            d.insert(k.delimiter.clone(), Object::from_char(delim));
            d.insert(
                k.quotechar.clone(),
                qc.map_or(Object::None, Object::from_char),
            );
            d.insert(
                k.escapechar.clone(),
                esc.map_or(Object::None, Object::from_char),
            );
            d.insert(k.lineterminator.clone(), Object::from_str(lineterm));
            d.insert(k.quoting.clone(), Object::Int(quot));
            d.insert(k.doublequote.clone(), Object::Bool(dq));
            d.insert(k.skipinitialspace.clone(), Object::Bool(sis));
            d.insert(k.strict.clone(), Object::Bool(strict_v));
        }
        Ok(Object::Instance(Rc::new(inst)))
    })
}

fn obj_truthy(o: &Object) -> bool {
    match o {
        Object::Bool(b) => *b,
        Object::None => false,
        Object::Int(n) => *n != 0,
        Object::Str(s) => !s.is_empty(),
        _ => true,
    }
}

fn set_bool(src: Option<&Object>, dflt: bool) -> bool {
    src.map_or(dflt, obj_truthy)
}

fn set_int(name: &str, src: Option<&Object>, dflt: i64) -> Result<i64, RuntimeError> {
    match src {
        None => Ok(dflt),
        Some(Object::Int(n)) => Ok(*n),
        Some(_) => Err(type_error(format!("\"{name}\" must be an integer"))),
    }
}

fn one_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

fn set_char(name: &str, src: Option<&Object>, dflt: char) -> Result<char, RuntimeError> {
    match src {
        None => Ok(dflt),
        Some(Object::Str(s)) => one_char(s).ok_or_else(|| {
            type_error(format!(
                "\"{name}\" must be a unicode character, not a string of length {}",
                s.chars().count()
            ))
        }),
        Some(other) => Err(type_error(format!(
            "\"{name}\" must be a unicode character, not {}",
            other.type_name()
        ))),
    }
}

fn set_char_or_none(
    name: &str,
    src: Option<&Object>,
    dflt: Option<char>,
) -> Result<Option<char>, RuntimeError> {
    match src {
        None => Ok(dflt),
        Some(Object::None) => Ok(None),
        Some(Object::Str(s)) => one_char(s).map(Some).ok_or_else(|| {
            type_error(format!(
                "\"{name}\" must be a unicode character or None, not a string of length {}",
                s.chars().count()
            ))
        }),
        Some(other) => Err(type_error(format!(
            "\"{name}\" must be a unicode character or None, not {}",
            other.type_name()
        ))),
    }
}

fn set_str(name: &str, src: Option<&Object>, dflt: &str) -> Result<Option<String>, RuntimeError> {
    match src {
        None => Ok(Some(dflt.to_owned())),
        Some(Object::Str(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(type_error(format!(
            "\"{name}\" must be a string, not {}",
            other.type_name()
        ))),
    }
}

fn check_quoting(quoting: i64) -> Result<(), RuntimeError> {
    if (QUOTE_MINIMAL..=QUOTE_NOTNULL).contains(&quoting) {
        Ok(())
    } else {
        Err(type_error("bad \"quoting\" value"))
    }
}

fn check_char(
    name: &str,
    c: Option<char>,
    allowspace: bool,
    lineterminator: &str,
) -> Result<(), RuntimeError> {
    if let Some(c) = c {
        if c == '\r' || c == '\n' || (c == ' ' && !allowspace) {
            return Err(value_error(format!("bad {name} value")));
        }
        if lineterminator.contains(c) {
            return Err(value_error(format!("bad {name} or lineterminator value")));
        }
    }
    Ok(())
}

fn check_chars(
    name1: &str,
    name2: &str,
    c1: Option<char>,
    c2: Option<char>,
) -> Result<(), RuntimeError> {
    if c1.is_some() && c1 == c2 {
        return Err(value_error(format!("bad {name1} or {name2} value")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dialect config (read back from a built Dialect instance)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DialectCfg {
    delimiter: char,
    quotechar: Option<char>,
    escapechar: Option<char>,
    lineterminator: String,
    quoting: i64,
    doublequote: bool,
    skipinitialspace: bool,
    strict: bool,
}

impl DialectCfg {
    /// Whether `f` holds none of the characters [`join_append`] quotes or
    /// escapes (an ASCII dialect's, checked a byte at a time).
    fn plain_field(&self, f: &str) -> bool {
        let ascii = |c: Option<char>| c.is_none_or(|c| c.is_ascii());
        if !(self.delimiter.is_ascii()
            && ascii(self.quotechar)
            && ascii(self.escapechar)
            && self.lineterminator.is_ascii())
        {
            return false;
        }
        let byte = |c: Option<char>| c.map_or(0, |c| c as u8);
        let (d, q, e) = (
            self.delimiter as u8,
            byte(self.quotechar),
            byte(self.escapechar),
        );
        let term = self.lineterminator.as_bytes();
        f.bytes().all(|b| {
            b != d
                && b != b'\n'
                && b != b'\r'
                && (b != q || q == 0)
                && (b != e || e == 0)
                && !term.contains(&b)
        })
    }
}

fn read_cfg(dialect: &Object) -> Result<DialectCfg, RuntimeError> {
    let inst = match dialect {
        Object::Instance(i) => i,
        _ => return Err(type_error("invalid dialect")),
    };
    let d = inst.dict_cell().borrow();
    let get = |k: &DictKey| d.get(k).cloned();
    let k = dialect_keys();
    let as_char = |o: Option<Object>| -> Option<char> {
        match o {
            Some(Object::Str(s)) => one_char(&s),
            _ => None,
        }
    };
    Ok(DialectCfg {
        delimiter: as_char(get(&k.delimiter)).unwrap_or(','),
        quotechar: as_char(get(&k.quotechar)),
        escapechar: as_char(get(&k.escapechar)),
        lineterminator: match get(&k.lineterminator) {
            Some(Object::Str(s)) => s.to_string(),
            _ => "\r\n".to_owned(),
        },
        quoting: match get(&k.quoting) {
            Some(Object::Int(n)) => n,
            _ => QUOTE_MINIMAL,
        },
        doublequote: matches!(get(&k.doublequote), Some(Object::Bool(true))),
        skipinitialspace: matches!(get(&k.skipinitialspace), Some(Object::Bool(true))),
        strict: matches!(get(&k.strict), Some(Object::Bool(true))),
    })
}

/// A reader's view of its dialect, packed for the reader's state: a dialect
/// never changes once built, so a reader reads its attributes once and each
/// row unpacks this instead. (The line terminator is the writer's business.)
fn pack_reader_cfg(cfg: &DialectCfg) -> Object {
    const NONE: u32 = u32::MAX;
    let mut out = Vec::with_capacity(21);
    out.extend_from_slice(&(cfg.delimiter as u32).to_le_bytes());
    out.extend_from_slice(&cfg.quotechar.map_or(NONE, |c| c as u32).to_le_bytes());
    out.extend_from_slice(&cfg.escapechar.map_or(NONE, |c| c as u32).to_le_bytes());
    out.extend_from_slice(&cfg.quoting.to_le_bytes());
    out.push(
        u8::from(cfg.doublequote) | u8::from(cfg.skipinitialspace) << 1 | u8::from(cfg.strict) << 2,
    );
    Object::new_bytes(out)
}

/// [`pack_reader_cfg`]'s inverse; `None` for anything it didn't make.
fn unpack_reader_cfg(packed: &Object) -> Option<DialectCfg> {
    let Object::Bytes(b) = packed else {
        return None;
    };
    let b: &[u8; 21] = (&b[..]).try_into().ok()?;
    let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    Some(DialectCfg {
        delimiter: char::from_u32(word(0))?,
        quotechar: char::from_u32(word(4)),
        escapechar: char::from_u32(word(8)),
        lineterminator: String::new(),
        quoting: i64::from_le_bytes(b[12..20].try_into().ok()?),
        doublequote: b[20] & 1 != 0,
        skipinitialspace: b[20] & 2 != 0,
        strict: b[20] & 4 != 0,
    })
}

/// Resolve a (possibly string / object / `None`) dialect argument plus
/// `**fmtparams` into a `_csv.Dialect` — the moral equivalent of
/// `_call_dialect`, routed through `Dialect.__new__` for the heavy
/// lifting so there is a single validation implementation.
fn call_dialect(
    interp: &mut Interpreter,
    dialect_pos: Option<Object>,
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mut call_args: Vec<Object> = Vec::new();
    if let Some(d) = dialect_pos {
        call_args.push(d);
    }
    interp.call_object(Object::Type(dialect_type()), &call_args, kwargs)
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum St {
    StartRecord,
    StartField,
    EscapedChar,
    InField,
    InQuotedField,
    EscapeInQuoted,
    QuoteInQuoted,
    EatCrnl,
    AfterEscapedCrnl,
}

/// How [`Parser::save_field`] converts an unquoted field to a `float` for
/// the numeric quoting styles: through `float` itself, or (for the leaf
/// half, which never reads with those styles) not at all.
trait FloatConv {
    fn to_float(&mut self, val: Object) -> Result<Object, RuntimeError>;
}

impl FloatConv for Interpreter {
    fn to_float(&mut self, val: Object) -> Result<Object, RuntimeError> {
        self.call_object(
            Object::Type(builtin_types().float_.clone()),
            std::slice::from_ref(&val),
            &[],
        )
    }
}

/// The leaf half's [`FloatConv`]: it declines the styles that convert.
struct NoFloats;

impl FloatConv for NoFloats {
    fn to_float(&mut self, _val: Object) -> Result<Object, RuntimeError> {
        Err(type_error("this reader converts no fields"))
    }
}

struct Parser {
    cfg: DialectCfg,
    state: St,
    field: String,
    field_len: i64,
    fields: Vec<Object>,
    unquoted: bool,
}

impl Parser {
    fn new(cfg: DialectCfg) -> Self {
        Parser {
            cfg,
            state: St::StartRecord,
            field: String::new(),
            field_len: 0,
            fields: Vec::new(),
            unquoted: false,
        }
    }

    fn add_char(&mut self, c: char) -> Result<(), RuntimeError> {
        let limit = FIELD_LIMIT.load(Ordering::Relaxed);
        if self.field_len >= limit {
            return Err(csv_error(format!(
                "field larger than field limit ({limit})"
            )));
        }
        self.field.push(c);
        self.field_len += 1;
        Ok(())
    }

    fn save_field(&mut self, interp: &mut impl FloatConv) -> Result<(), RuntimeError> {
        let q = self.cfg.quoting;
        if self.unquoted && self.field_len == 0 && (q == QUOTE_NOTNULL || q == QUOTE_STRINGS) {
            self.fields.push(Object::None);
        } else {
            let mut val = crate::builtins::bridge_to_object(&self.field);
            if self.unquoted && self.field_len != 0 && (q == QUOTE_NONNUMERIC || q == QUOTE_STRINGS)
            {
                val = interp.to_float(val)?;
            }
            self.fields.push(val);
        }
        self.field.clear();
        self.field_len = 0;
        Ok(())
    }

    /// How many bytes at the front of `rest` are ordinary characters of the
    /// field being read, which go into it as they are: up to the next
    /// character the current state gives a meaning to. Zero when the state
    /// isn't inside a field, when the dialect's own characters aren't ASCII
    /// (the scan is by byte), or when the run would pass the field limit
    /// (the character-at-a-time path raises that at the right place).
    fn plain_run(&self, rest: &[u8]) -> usize {
        let ascii = |c: char| c.is_ascii().then_some(c as u8);
        let escape = match self.cfg.escapechar {
            None => None,
            Some(c) => match ascii(c) {
                Some(b) => Some(b),
                None => return 0,
            },
        };
        match self.state {
            St::InField => {
                let Some(delimiter) = ascii(self.cfg.delimiter) else {
                    return 0;
                };
                match escape {
                    None => memchr::memchr3(delimiter, b'\n', b'\r', rest),
                    Some(e) => rest
                        .iter()
                        .position(|&b| b == delimiter || b == b'\n' || b == b'\r' || b == e),
                }
            }
            St::InQuotedField => {
                let quote = match self.cfg.quotechar {
                    Some(q) if self.cfg.quoting != QUOTE_NONE => match ascii(q) {
                        Some(b) => Some(b),
                        None => return 0,
                    },
                    _ => None,
                };
                match (quote, escape) {
                    (None, None) => None,
                    (Some(a), None) | (None, Some(a)) => memchr::memchr(a, rest),
                    (Some(a), Some(b)) => memchr::memchr2(a, b, rest),
                }
            }
            _ => return 0,
        }
        .unwrap_or(rest.len())
    }

    /// Take the `run` of ordinary characters [`Parser::plain_run`] found
    /// into the field. `false`, with nothing taken, when that would pass the
    /// field limit.
    fn add_run(&mut self, run: &str) -> bool {
        let chars = if run.is_ascii() {
            run.len()
        } else {
            run.chars().count()
        } as i64;
        if self.field_len + chars > FIELD_LIMIT.load(Ordering::Relaxed) {
            return false;
        }
        self.field.push_str(run);
        self.field_len += chars;
        true
    }

    /// Run one line through the machine, then its end-of-line marker.
    /// Runs of a field's ordinary characters go in whole; the characters
    /// that mean something go through [`Parser::process`].
    fn feed_line(&mut self, interp: &mut impl FloatConv, line: &str) -> Result<(), RuntimeError> {
        let mut at = 0;
        while at < line.len() {
            let run = self.plain_run(&line.as_bytes()[at..]);
            if run > 0 && self.add_run(&line[at..at + run]) {
                at += run;
                continue;
            }
            let ch = line[at..].chars().next().expect("at a character");
            self.process(interp, Some(ch))?;
            at += ch.len_utf8();
        }
        self.process(interp, None)
    }

    fn is_quote(&self, c: Option<char>) -> bool {
        self.cfg.quotechar.is_some_and(|q| c == Some(q))
    }

    fn is_escape(&self, c: Option<char>) -> bool {
        self.cfg.escapechar.is_some_and(|e| c == Some(e))
    }

    fn process(
        &mut self,
        interp: &mut impl FloatConv,
        c: Option<char>,
    ) -> Result<(), RuntimeError> {
        match self.state {
            St::StartRecord => {
                if c.is_none() {
                    return Ok(());
                }
                if matches!(c, Some('\n') | Some('\r')) {
                    self.state = St::EatCrnl;
                    return Ok(());
                }
                self.state = St::StartField;
                self.process_start_field(interp, c)?;
            }
            St::StartField => self.process_start_field(interp, c)?,
            St::EscapedChar => {
                if matches!(c, Some('\n') | Some('\r')) {
                    self.add_char(c.unwrap())?;
                    self.state = St::AfterEscapedCrnl;
                    return Ok(());
                }
                let ch = c.unwrap_or('\n');
                self.add_char(ch)?;
                self.state = St::InField;
            }
            St::AfterEscapedCrnl => {
                if c.is_none() {
                    return Ok(());
                }
                self.process_in_field(interp, c)?;
            }
            St::InField => self.process_in_field(interp, c)?,
            St::InQuotedField => {
                if c.is_none() {
                    // ignore embedded EOL marker
                } else if self.is_escape(c) {
                    self.state = St::EscapeInQuoted;
                } else if self.is_quote(c) && self.cfg.quoting != QUOTE_NONE {
                    if self.cfg.doublequote {
                        self.state = St::QuoteInQuoted;
                    } else {
                        self.state = St::InField;
                    }
                } else if let Some(ch) = c {
                    self.add_char(ch)?;
                }
            }
            St::EscapeInQuoted => {
                let ch = c.unwrap_or('\n');
                self.add_char(ch)?;
                self.state = St::InQuotedField;
            }
            St::QuoteInQuoted => {
                if self.cfg.quoting != QUOTE_NONE && self.is_quote(c) {
                    self.add_char(c.unwrap())?;
                    self.state = St::InQuotedField;
                } else if c == Some(self.cfg.delimiter) {
                    self.save_field(interp)?;
                    self.state = St::StartField;
                } else if matches!(c, None | Some('\n') | Some('\r')) {
                    self.save_field(interp)?;
                    self.state = if c.is_none() {
                        St::StartRecord
                    } else {
                        St::EatCrnl
                    };
                } else if !self.cfg.strict {
                    self.add_char(c.unwrap())?;
                    self.state = St::InField;
                } else {
                    return Err(csv_error(format!(
                        "'{}' expected after '{}'",
                        self.cfg.delimiter,
                        self.cfg.quotechar.unwrap_or('"')
                    )));
                }
            }
            St::EatCrnl => {
                if matches!(c, Some('\n') | Some('\r')) {
                    // swallow
                } else if c.is_none() {
                    self.state = St::StartRecord;
                } else {
                    return Err(csv_error(
                        "new-line character seen in unquoted field - do you need to \
                         open the file with newline=''?",
                    ));
                }
            }
        }
        Ok(())
    }

    fn process_start_field(
        &mut self,
        interp: &mut impl FloatConv,
        c: Option<char>,
    ) -> Result<(), RuntimeError> {
        self.unquoted = true;
        if matches!(c, None | Some('\n') | Some('\r')) {
            self.save_field(interp)?;
            self.state = if c.is_none() {
                St::StartRecord
            } else {
                St::EatCrnl
            };
        } else if self.is_quote(c) && self.cfg.quoting != QUOTE_NONE {
            self.unquoted = false;
            self.state = St::InQuotedField;
        } else if self.is_escape(c) {
            self.state = St::EscapedChar;
        } else if c == Some(' ') && self.cfg.skipinitialspace {
            // ignore leading spaces
        } else if c == Some(self.cfg.delimiter) {
            self.save_field(interp)?;
        } else {
            self.add_char(c.unwrap())?;
            self.state = St::InField;
        }
        Ok(())
    }

    fn process_in_field(
        &mut self,
        interp: &mut impl FloatConv,
        c: Option<char>,
    ) -> Result<(), RuntimeError> {
        if matches!(c, None | Some('\n') | Some('\r')) {
            self.save_field(interp)?;
            self.state = if c.is_none() {
                St::StartRecord
            } else {
                St::EatCrnl
            };
        } else if self.is_escape(c) {
            self.state = St::EscapedChar;
        } else if c == Some(self.cfg.delimiter) {
            self.save_field(interp)?;
            self.state = St::StartField;
        } else {
            self.add_char(c.unwrap())?;
        }
        Ok(())
    }
}

fn self_inst(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("expected an instance as the first argument")),
    }
}

fn str_chars(o: &Object) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match o {
        Object::Str(s) => Some(Cow::Borrowed(s)),
        // Lone surrogates (e.g. a file read with `errors="surrogatepass"`)
        // ride through the DFA bridged into the PUA window; `save_field`
        // un-bridges them so fields keep the surrogate code points.
        Object::WStr(cps) => Some(Cow::Owned(crate::builtins::bridge_encode_cps(cps))),
        _ => None,
    }
}

fn csv_reader(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    with_interp(|interp| {
        if args.is_empty() {
            return Err(type_error("reader() argument 1 must support iteration"));
        }
        if args.len() > 2 {
            return Err(type_error("reader expected at most 2 arguments"));
        }
        let iterable = args[0].clone();
        let dialect_pos = args.get(1).cloned();
        let input_iter = interp.iter_object(iterable)?;
        // (No dialect and no overrides: the default dialect, immutable,
        // built once with its packed view.)
        let (dialect, cfg) = if dialect_pos.is_none() && kwargs.is_empty() {
            default_dialect(interp)?
        } else {
            let dialect = call_dialect(interp, dialect_pos, kwargs)?;
            let cfg = pack_reader_cfg(&read_cfg(&dialect)?);
            (dialect, cfg)
        };
        let inst = PyInstance::new(reader_type());
        {
            let keys = reader_keys();
            let mut d = inst.dict_cell().borrow_mut();
            d.insert(keys.input_iter.clone(), input_iter);
            d.insert(dialect_keys().dialect.clone(), dialect);
            d.insert(keys.cfg.clone(), cfg);
            d.insert(keys.line_num.clone(), Object::Int(0));
            // (No `__next__` in progress, so the leaf half reads the first
            // row too.)
            d.insert(keys.fields_live.clone(), Object::Bool(false));
        }
        Ok(Object::Instance(Rc::new(inst)))
    })
}

thread_local! {
    /// The default `Dialect()` and its packed reader view (see
    /// [`default_dialect`]).
    static DEFAULT_DIALECT: crate::gil::ReleaseUnderGil<RefCell<Option<(Object, Object)>>> =
        const { crate::gil::ReleaseUnderGil::new(RefCell::new(None)) };
}

/// The dialect a reader made with no dialect and no format parameters
/// reads by (CPython builds an equal one each time; a dialect never
/// changes once built), and its packed view.
fn default_dialect(interp: &mut Interpreter) -> Result<(Object, Object), RuntimeError> {
    if let Some(hit) = DEFAULT_DIALECT.with(|d| d.borrow().clone()) {
        return Ok(hit);
    }
    let dialect = call_dialect(interp, None, &[])?;
    let cfg = pack_reader_cfg(&read_cfg(&dialect)?);
    DEFAULT_DIALECT.with(|d| *d.borrow_mut() = Some((dialect.clone(), cfg.clone())));
    Ok((dialect, cfg))
}

fn reader_iter_self(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("__iter__ requires self"))
}

fn reader_disallow_new(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot create '_csv.reader' instances"))
}

fn reader_iternext(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    with_interp(|interp| {
        let keys = reader_keys();
        let (input_iter, cfg) = {
            let d = inst.dict_cell().borrow();
            let cfg = match d.get(&keys.cfg).and_then(unpack_reader_cfg) {
                Some(cfg) => cfg,
                None => {
                    let dialect = d
                        .get(&dialect_keys().dialect)
                        .ok_or_else(|| type_error("reader is missing its dialect"))?;
                    read_cfg(dialect)?
                }
            };
            (d.get(&keys.input_iter).cloned(), cfg)
        };
        let input_iter =
            input_iter.ok_or_else(|| type_error("reader is missing its input iterator"))?;
        let mut p = Parser::new(cfg);
        // CPython's `parse_reset` allocates `self->fields`; a successful
        // `__next__` hands it out and leaves NULL behind. A re-entrant
        // `next(reader)` from inside the input iterator that completes
        // therefore makes the outer call see NULL and raise (gh-145105).
        set_reader_value(&inst, &keys.fields_live, Object::Bool(true));
        loop {
            match interp.iter_next_object(input_iter.clone())? {
                None => {
                    if p.field_len != 0 || p.state == St::InQuotedField {
                        if p.cfg.strict {
                            return Err(csv_error("unexpected end of data"));
                        }
                        p.save_field(interp)?;
                        break;
                    }
                    return Err(stop_iteration());
                }
                Some(obj) => {
                    let line = match str_chars(&obj) {
                        Some(s) => s,
                        None => {
                            return Err(csv_error(format!(
                                "iterator should return strings, not {} \
                                 (the file should be opened in text mode)",
                                obj.type_name()
                            )))
                        }
                    };
                    {
                        let mut d = inst.dict_cell().borrow_mut();
                        if !matches!(d.get(&keys.fields_live), Some(Object::Bool(true))) {
                            return Err(csv_error("iterator has already advanced the reader"));
                        }
                        // (Updated in place: the key is there after the
                        // first line.)
                        match d.get_mut(&keys.line_num) {
                            Some(v @ Object::Int(_)) => {
                                let Object::Int(n) = *v else { unreachable!() };
                                *v = Object::Int(n + 1);
                            }
                            _ => {
                                d.insert(keys.line_num.clone(), Object::Int(1));
                            }
                        }
                    }
                    p.feed_line(interp, &line)?;
                }
            }
            if p.state == St::StartRecord {
                break;
            }
        }
        set_reader_value(&inst, &keys.fields_live, Object::Bool(false));
        Ok(Object::new_list(std::mem::take(&mut p.fields)))
    })
}

/// The reader's `__next__` leaf half: a line that is one whole record,
/// read without the dispatch loop's full call, from a list or tuple
/// iterator or from a text stream that has the line in its buffer (see
/// `PyFile::next_line_pure`). A line of plain and quoted fields is split
/// directly (see [`split_direct`]); any other runs through the machine. A
/// record that goes on past its line, a line the machine raises on, a line
/// that isn't a plain `str`, a quoting style that converts fields to
/// `float`, a reader in the middle of a call, and
/// any other input decline to the full parser, which then reads the same
/// line.
/// A reader's state entries, by the position `csv_reader` inserts them at.
const AT_INPUT_ITER: usize = 0;
const AT_CFG: usize = 2;
const AT_LINE_NUM: usize = 3;
const AT_FIELDS_LIVE: usize = 4;

/// Whether `a` is the very key `b` (the same interned name).
fn same_key(a: &DictKey, b: &DictKey) -> bool {
    matches!((&a.0, &b.0), (Object::Str(x), Object::Str(y)) if crate::shared_value::SharedStr::ptr_eq(x, y))
}

/// A reader's state entry `key`, read at position `at` while that still
/// holds it (the layout `csv_reader` builds), else found by hash.
fn state_get<'a>(d: &'a DictData, at: usize, key: &DictKey) -> Option<&'a Object> {
    match d.get_index(at) {
        Some((k, v)) if same_key(k, key) => Some(v),
        _ => d.get(key),
    }
}

fn reader_next_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(inst)] = args else {
        return None;
    };
    let keys = reader_keys();
    let mut d = inst.dict_cell().try_borrow_mut().ok()?;
    let src = match state_get(&d, AT_INPUT_ITER, &keys.input_iter) {
        Some(o @ Object::Iter(_)) => o.clone(),
        Some(o @ Object::File(f)) if !f.binary => o.clone(),
        _ => return None,
    };
    if !matches!(
        state_get(&d, AT_FIELDS_LIVE, &keys.fields_live),
        Some(Object::Bool(false))
    ) || !matches!(
        state_get(&d, AT_LINE_NUM, &keys.line_num),
        Some(Object::Int(_))
    ) {
        return None;
    }
    let cfg = state_get(&d, AT_CFG, &keys.cfg).and_then(unpack_reader_cfg)?;
    if matches!(cfg.quoting, QUOTE_NONNUMERIC | QUOTE_STRINGS) {
        return None;
    }
    let fields = match &src {
        Object::Iter(it) => {
            let mut it = it.try_borrow_mut().ok()?;
            // (`None` at the source's end, with no record begun: the end.)
            let next = match &mut *it {
                crate::object::PyIterator::List {
                    items,
                    index,
                    owner: None,
                } => {
                    let items = items.try_borrow().ok()?;
                    match items.get(*index) {
                        Some(Object::Str(s)) => Some((s.clone(), index)),
                        None => None,
                        _ => return None,
                    }
                }
                crate::object::PyIterator::Tuple { items, index } => match items.get(*index) {
                    Some(Object::Str(s)) => Some((s.clone(), index)),
                    None => None,
                    _ => return None,
                },
                _ => return None,
            };
            let Some((line, index)) = next else {
                // The step that finds the end, as the full path's leaves the
                // iterator, then the end itself.
                let end = it.next_value();
                debug_assert!(end.is_none());
                drop(end);
                return Some(Err(stop_iteration()));
            };
            let fields = record_fields(cfg, &line)?;
            *index += 1;
            fields
        }
        Object::File(f) => {
            let fields = f.next_line_pure_with(None, |line| match &line {
                Object::Str(s) => record_fields(cfg, s),
                _ => None,
            })?;
            // (As a text stream's own `__next__` leaves it.)
            f.telling.set(false);
            fields
        }
        _ => return None,
    };
    let line_num = match d.get_index_mut(AT_LINE_NUM) {
        Some((k, v)) if same_key(k, &keys.line_num) => Some(v),
        _ => d.get_mut(&keys.line_num),
    };
    if let Some(Object::Int(n)) = line_num {
        *n += 1;
    }
    Some(Ok(Object::new_list(fields)))
}

/// The fields of `line` when it is one whole record the machine reads
/// without raising (and without converting fields, which `cfg` doesn't).
fn record_fields(cfg: DialectCfg, line: &str) -> Option<Vec<Object>> {
    if let Some(fields) = split_direct(&cfg, line) {
        return Some(fields);
    }
    let mut p = Parser::new(cfg);
    // (Sized once: no field is longer than its line, and a record has at
    // most one field more than the line has delimiters.)
    p.field.reserve(line.len());
    if p.cfg.delimiter.is_ascii() {
        let d = p.cfg.delimiter as u8;
        p.fields
            .reserve(memchr::memchr_iter(d, line.as_bytes()).count() + 1);
    }
    p.feed_line(&mut NoFloats, line).ok()?;
    (p.state == St::StartRecord).then_some(p.fields)
}

/// `line`'s fields read straight off it, which is what the machine makes
/// of it: for a dialect with ASCII characters, no escape in the line, no
/// `skipinitialspace`, and a quoting style that leaves unquoted fields as
/// they are, a line that is one whole record of unquoted fields (in which a
/// quote is an ordinary character) and quoted ones (doubled quotes inside
/// them read as one, when the dialect doubles), each quoted field closed
/// before the next delimiter, and no carriage return or newline outside a
/// quoted field before the line's own terminator. `None` for any other line
/// (a record that goes on past its line, a line the machine raises on or
/// reads characters after a closing quote into, an empty line, one longer
/// than the field limit), which the machine then reads.
fn split_direct(cfg: &DialectCfg, line: &str) -> Option<Vec<Object>> {
    if !matches!(cfg.quoting, QUOTE_MINIMAL | QUOTE_ALL | QUOTE_NONE) || cfg.skipinitialspace {
        return None;
    }
    let ascii = |c: char| c.is_ascii().then_some(c as u8);
    let delimiter = ascii(cfg.delimiter).filter(|&b| b != b'\r' && b != b'\n')?;
    let quote = match cfg.quotechar {
        Some(q) if cfg.quoting != QUOTE_NONE => {
            Some(ascii(q).filter(|&q| q != delimiter && q != b'\r' && q != b'\n')?)
        }
        _ => None,
    };
    let bytes = line.as_bytes();
    let body = match bytes {
        [.., b'\r', b'\n'] => bytes.len() - 2,
        [.., b'\n' | b'\r'] => bytes.len() - 1,
        _ => bytes.len(),
    };
    if body == 0 || body as i64 > FIELD_LIMIT.load(Ordering::Relaxed) {
        return None;
    }
    let b = &bytes[..body];
    if let Some(e) = cfg.escapechar {
        if memchr::memchr(ascii(e)?, b).is_some() {
            return None;
        }
    }
    // (Every cut is at an ASCII byte, so every piece is whole text.)
    let plain = b.is_ascii();
    let text = |s: &str| {
        if plain {
            Object::from_str(s)
        } else {
            crate::builtins::bridge_to_object(s)
        }
    };
    let mut fields = Vec::with_capacity(memchr::memchr_iter(delimiter, b).count() + 1);
    let mut at = 0;
    loop {
        if let Some(q) = quote.filter(|&q| b.get(at) == Some(&q)) {
            let mut from = at + 1;
            let mut doubled = String::new();
            let close = loop {
                // (No closing quote: the record goes on past the line.)
                let next = from + memchr::memchr(q, &b[from..])?;
                if b.get(next + 1) != Some(&q) {
                    break next;
                }
                if !cfg.doublequote {
                    return None;
                }
                doubled.push_str(&line[from..=next]);
                from = next + 2;
            };
            if doubled.is_empty() {
                fields.push(text(&line[from..close]));
            } else {
                doubled.push_str(&line[from..close]);
                fields.push(text(&doubled));
            }
            at = close + 1;
            match b.get(at) {
                None => return Some(fields),
                Some(&c) if c == delimiter => at += 1,
                Some(_) => return None,
            }
        } else {
            match memchr::memchr3(delimiter, b'\r', b'\n', &b[at..]) {
                None => {
                    fields.push(text(&line[at..body]));
                    return Some(fields);
                }
                Some(n) if b[at + n] == delimiter => {
                    fields.push(text(&line[at..at + n]));
                    at += n + 1;
                }
                Some(_) => return None,
            }
        }
        // (A delimiter that ends the line leaves an empty last field.)
        if at == body {
            fields.push(text(""));
            return Some(fields);
        }
    }
}

/// `inst.<key> = value` on a reader's state: replaced in place when it is
/// there (every row's), inserted otherwise.
fn set_reader_value(inst: &PyInstance, key: &DictKey, value: Object) {
    let mut d = inst.dict_cell().borrow_mut();
    match d.get_mut(key) {
        Some(v) => {
            let old = std::mem::replace(v, value);
            drop(d);
            drop(old);
        }
        None => {
            d.insert(key.clone(), value);
        }
    }
}

fn reader_type() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = builtin_types();
        let mut dict = DictData::default();
        dict.insert(key("__iter__"), method("__iter__", reader_iter_self));
        let next = method("__next__", reader_iternext);
        if let Object::Builtin(b) = &next {
            crate::leaf_builtins::register_fast(b, reader_next_fast);
        }
        dict.insert(key("__next__"), next);
        dict.insert(
            key("__new__"),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(reader_disallow_new),
                call_kw: None,
            })),
        );
        TypeObject::new_user("_csv.reader", vec![bt.object_.clone()], dict)
            .expect("_csv.reader must linearise")
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

fn is_callable(o: &Object) -> bool {
    match o {
        Object::Function(_)
        | Object::Builtin(_)
        | Object::BoundMethod(_)
        | Object::Type(_)
        | Object::Generator(_)
        | Object::StaticMethod(_)
        | Object::ClassMethod(_) => true,
        Object::Instance(inst) => inst.cls().lookup("__call__").is_some(),
        _ => false,
    }
}

fn csv_writer(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    with_interp(|interp| {
        if args.is_empty() {
            return Err(type_error(
                "writer() argument 1 must have a \"write\" method",
            ));
        }
        if args.len() > 2 {
            return Err(type_error("writer expected at most 2 arguments"));
        }
        let output = args[0].clone();
        let dialect_pos = args.get(1).cloned();
        let write = match interp.load_attr(&output, "write") {
            Ok(w) => w,
            Err(RuntimeError::PyException(e)) if e.type_name() == "AttributeError" => {
                return Err(type_error("argument 1 must have a \"write\" method"))
            }
            Err(e) => return Err(e),
        };
        if !is_callable(&write) {
            return Err(type_error("argument 1 must have a \"write\" method"));
        }
        let dialect = call_dialect(interp, dialect_pos, kwargs)?;
        let inst = PyInstance::new(writer_type());
        {
            let mut d = inst.dict_cell().borrow_mut();
            d.insert(key("write"), write);
            d.insert(dialect_keys().dialect.clone(), dialect);
        }
        Ok(Object::Instance(Rc::new(inst)))
    })
}

fn writer_disallow_new(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot create '_csv.writer' instances"))
}

/// Number predicate for `QUOTE_NONNUMERIC` (`PyNumber_Check`).
fn is_number(o: &Object) -> bool {
    match o {
        Object::Int(_)
        | Object::Long(_)
        | Object::Float(_)
        | Object::Complex(_)
        | Object::Bool(_) => true,
        Object::Instance(_) => {
            crate::instance_method(o, "__index__").is_some()
                || crate::instance_method(o, "__int__").is_some()
                || crate::instance_method(o, "__float__").is_some()
        }
        _ => false,
    }
}

/// Append one field to the in-progress record, applying quoting/escaping
/// (a one-pass port of CPython's `join_append` / `join_append_data`).
fn join_append(
    rec: &mut String,
    cfg: &DialectCfg,
    num_fields: i64,
    field: Option<&str>,
    quoted_in: bool,
) -> Result<(), RuntimeError> {
    // A field with nothing to quote or escape (the common case) is copied
    // whole.
    if let Some(f) = field {
        if !f.is_empty() && cfg.plain_field(f) {
            if num_fields > 0 {
                rec.push(cfg.delimiter);
            }
            if quoted_in {
                rec.push(cfg.quotechar.unwrap_or('"'));
            }
            rec.push_str(f);
            if quoted_in {
                rec.push(cfg.quotechar.unwrap_or('"'));
            }
            return Ok(());
        }
    }
    let field_len = field.map_or(0, |f| f.chars().count());
    let mut quoted = quoted_in;
    if field_len == 0 && cfg.delimiter == ' ' && cfg.skipinitialspace {
        if cfg.quoting == QUOTE_NONE
            || (field.is_none() && (cfg.quoting == QUOTE_STRINGS || cfg.quoting == QUOTE_NOTNULL))
        {
            return Err(csv_error(
                "empty field must be quoted if delimiter is a space and skipinitialspace is true",
            ));
        }
        quoted = true;
    }

    let mut body = String::new();
    if let Some(f) = field {
        for c in f.chars() {
            let is_special = c == cfg.delimiter
                || cfg.escapechar == Some(c)
                || cfg.quotechar == Some(c)
                || c == '\n'
                || c == '\r'
                || cfg.lineterminator.contains(c);
            let mut want_escape = false;
            if is_special {
                if cfg.quoting == QUOTE_NONE {
                    want_escape = true;
                } else {
                    if cfg.quotechar == Some(c) {
                        if cfg.doublequote {
                            body.push(c);
                        } else {
                            want_escape = true;
                        }
                    } else if cfg.escapechar == Some(c) {
                        want_escape = true;
                    }
                    if !want_escape {
                        quoted = true;
                    }
                }
                if want_escape {
                    match cfg.escapechar {
                        Some(e) => body.push(e),
                        None => {
                            return Err(csv_error("need to escape, but no escapechar set"));
                        }
                    }
                }
            }
            body.push(c);
        }
    }

    if num_fields > 0 {
        rec.push(cfg.delimiter);
    }
    if quoted {
        rec.push(cfg.quotechar.unwrap_or('"'));
    }
    rec.push_str(&body);
    if quoted {
        rec.push(cfg.quotechar.unwrap_or('"'));
    }
    Ok(())
}

/// A writer's `write` method and its dialect's settings.
fn writer_parts(inst: &Rc<PyInstance>) -> Result<(Object, DialectCfg), RuntimeError> {
    let (write, dialect_obj) = {
        let d = inst.dict_cell().borrow();
        (
            d.get(&key("write")).cloned(),
            d.get(&dialect_keys().dialect).cloned(),
        )
    };
    let write = write.ok_or_else(|| type_error("writer is missing its write method"))?;
    let dialect_obj = dialect_obj.ok_or_else(|| type_error("writer is missing its dialect"))?;
    Ok((write, writer_cfg(&dialect_obj)?))
}

thread_local! {
    /// The last dialect a writer read (by address and its dict's stamp)
    /// and its settings: a writer's `writerow` reads them every row.
    static WRITER_CFG: std::cell::RefCell<Option<(usize, u64, DialectCfg)>> =
        const { std::cell::RefCell::new(None) };
}

/// [`read_cfg`] for a writer, through [`WRITER_CFG`]: the dialect's dict
/// unchanged since (its stamp moves with every store) means the same
/// settings.
fn writer_cfg(dialect: &Object) -> Result<DialectCfg, RuntimeError> {
    let Object::Instance(inst) = dialect else {
        return read_cfg(dialect);
    };
    let at = Rc::as_ptr(inst) as usize;
    let stamp = inst.dict_cell().borrow().mutation_stamp();
    let hit = WRITER_CFG.with(|c| {
        c.try_borrow().ok().and_then(|c| match &*c {
            Some((a, st, cfg)) if *a == at && *st == stamp => Some(cfg.clone()),
            _ => None,
        })
    });
    if let Some(cfg) = hit {
        return Ok(cfg);
    }
    let cfg = read_cfg(dialect)?;
    WRITER_CFG.with(|c| {
        if let Ok(mut c) = c.try_borrow_mut() {
            *c = Some((at, stamp, cfg.clone()));
        }
    });
    Ok(cfg)
}

fn writer_writerow_inner(
    interp: &mut Interpreter,
    write: &Object,
    cfg: &DialectCfg,
    seq: Object,
) -> Result<Object, RuntimeError> {
    let mut rec = String::new();
    let mut num_fields: i64 = 0;
    let mut last_null = false;
    let mut emit = |interp: &mut Interpreter, field: &Object| -> Result<(), RuntimeError> {
        let quoted = match cfg.quoting {
            QUOTE_NONNUMERIC => !is_number(field),
            QUOTE_ALL => true,
            QUOTE_STRINGS => matches!(field, Object::Str(_) | Object::WStr(_)),
            QUOTE_NOTNULL => !matches!(field, Object::None),
            _ => false,
        };
        last_null = matches!(field, Object::None);
        if let Some(s) = str_chars(field) {
            join_append(&mut rec, cfg, num_fields, Some(&s), quoted)?;
        } else if last_null {
            join_append(&mut rec, cfg, num_fields, None, quoted)?;
        } else {
            // (`str()` of a plain number or bool is its own text.)
            let text = match field {
                Object::Int(n) => Some(itoa::Buffer::new().format(*n).to_owned()),
                Object::Float(x) => Some(crate::object::float_repr(*x)),
                Object::Bool(b) => Some(if *b { "True" } else { "False" }.to_owned()),
                _ => None,
            };
            match text {
                Some(t) => join_append(&mut rec, cfg, num_fields, Some(&t), quoted)?,
                None => {
                    let s = interp.call_object(
                        Object::Type(builtin_types().str_.clone()),
                        std::slice::from_ref(field),
                        &[],
                    )?;
                    let s = str_chars(&s).unwrap_or_default();
                    join_append(&mut rec, cfg, num_fields, Some(&s), quoted)?;
                }
            }
        }
        num_fields += 1;
        Ok(())
    };

    // A list or tuple of plain strings, numbers, bools and `None`s: its
    // items in order, read at once (writing them runs no code, so
    // iterating the sequence live would see the same items).
    let plain = |x: &Object| {
        matches!(
            x,
            Object::Str(_) | Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
        )
    };
    let items = match &seq {
        Object::List(l) => l
            .try_borrow()
            .ok()
            .filter(|v| v.iter().all(plain))
            .map(|v| v.clone()),
        Object::Tuple(t) => t.iter().all(plain).then(|| t.to_vec()),
        _ => None,
    };
    if let Some(items) = items {
        for field in &items {
            emit(interp, field)?;
        }
    } else {
        let iter = match interp.iter_object(seq.clone()) {
            Ok(it) => it,
            Err(RuntimeError::PyException(e)) if e.type_name() == "TypeError" => {
                return Err(csv_error(format!(
                    "iterable expected, not {}",
                    seq.type_name()
                )))
            }
            Err(e) => return Err(e),
        };
        while let Some(field) = interp.iter_next_object(iter.clone())? {
            emit(interp, &field)?;
        }
    }
    drop(emit);

    if num_fields > 0 && rec.is_empty() {
        if cfg.quoting == QUOTE_NONE
            || (last_null && (cfg.quoting == QUOTE_STRINGS || cfg.quoting == QUOTE_NOTNULL))
        {
            return Err(csv_error("single empty field record must be quoted"));
        }
        join_append(&mut rec, cfg, num_fields - 1, None, true)?;
    }

    rec.push_str(&cfg.lineterminator);
    let line = Object::from_str(rec);
    interp.call_object(write.clone(), &[line], &[])
}

fn writer_writerow(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let seq = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("writerow() takes exactly one argument (0 given)"))?;
    let (write, cfg) = writer_parts(&inst)?;
    with_interp(|interp| writer_writerow_inner(interp, &write, &cfg, seq))
}

fn writer_writerows(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_inst(args)?;
    let seqseq = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("writerows() takes exactly one argument (0 given)"))?;
    // (CPython's writer holds its `write` and dialect from creation; both
    // are read-only attributes there.)
    let (write, cfg) = writer_parts(&inst)?;
    with_interp(|interp| {
        let row_iter = interp.iter_object(seqseq)?;
        loop {
            let row = match interp.iter_next_object(row_iter.clone())? {
                Some(r) => r,
                None => break,
            };
            writer_writerow_inner(interp, &write, &cfg, row)?;
        }
        Ok(Object::None)
    })
}

fn writer_type() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = builtin_types();
        let mut dict = DictData::default();
        dict.insert(key("writerow"), method("writerow", writer_writerow));
        dict.insert(key("writerows"), method("writerows", writer_writerows));
        dict.insert(
            key("__new__"),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(writer_disallow_new),
                call_kw: None,
            })),
        );
        TypeObject::new_user("_csv.writer", vec![bt.object_.clone()], dict)
            .expect("_csv.writer must linearise")
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Dialect registry + field size limit
// ---------------------------------------------------------------------------

fn csv_register_dialect(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    with_interp(|interp| {
        let name = match args.first() {
            Some(o @ Object::Str(_)) => o.clone(),
            _ => return Err(type_error("dialect name must be a string")),
        };
        if args.len() > 2 {
            return Err(type_error(
                "register_dialect() takes at most 2 positional arguments",
            ));
        }
        let dialect_pos = args.get(1).cloned();
        let dialect = call_dialect(interp, dialect_pos, kwargs)?;
        dialects().borrow_mut().insert(DictKey(name), dialect);
        Ok(Object::None)
    })
}

fn csv_unregister_dialect(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 1 {
        return Err(type_error(
            "unregister_dialect() takes exactly one argument",
        ));
    }
    let removed = dialects()
        .borrow_mut()
        .shift_remove(&DictKey(args[0].clone()));
    if removed.is_none() {
        return Err(csv_error("unknown dialect"));
    }
    Ok(Object::None)
}

fn csv_get_dialect(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 1 {
        return Err(type_error("get_dialect() takes exactly one argument"));
    }
    let found = dialects().borrow().get(&DictKey(args[0].clone())).cloned();
    found.ok_or_else(|| csv_error("unknown dialect"))
}

fn csv_list_dialects(args: &[Object]) -> Result<Object, RuntimeError> {
    if !args.is_empty() {
        return Err(type_error("list_dialects() takes no arguments"));
    }
    let keys: Vec<Object> = dialects().borrow().keys().map(|k| k.0.clone()).collect();
    Ok(Object::new_list(keys))
}

fn csv_field_size_limit(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error("field_size_limit() takes at most 1 argument"));
    }
    let old = FIELD_LIMIT.load(Ordering::Relaxed);
    if let Some(v) = args.first() {
        match v {
            Object::Int(n) => FIELD_LIMIT.store(*n, Ordering::Relaxed),
            _ => return Err(type_error("limit must be an integer")),
        }
    }
    Ok(Object::Int(old))
}
