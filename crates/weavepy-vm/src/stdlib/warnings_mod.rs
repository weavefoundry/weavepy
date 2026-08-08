//! The `_warnings` module — a faithful port of CPython's
//! `Modules/_warnings.c` (RFC 0056 WS4).
//!
//! The verbatim `Lib/warnings.py` does `from _warnings import (filters,
//! _defaultaction, _onceregistry, warn, warn_explicit, _filters_mutated)`
//! and, when that succeeds, the C module owns the filter state and the
//! whole warn pipeline; the Python file only supplies display hooks and
//! the `catch_warnings` bookkeeping. Like CPython, this module keeps a
//! *last-known* internal state (filters list, once-registry, default
//! action, filters version) and re-reads the live `warnings` module
//! attributes on every use, so `del warnings.filters` degrades exactly
//! the way `test_warnings._WarningsTests` asserts.

use std::sync::Mutex;

use crate::error::{runtime_error, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{member_eq, BuiltinFn, DictData, DictKey, Object, PyModule, StrKey};
use crate::stdlib::sqlite3_native::is_callable;
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::types::TypeObject;

pub(crate) type Interp = crate::Interpreter;

fn interp<'a>() -> Result<&'a mut Interp, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| RuntimeError::Internal("_warnings: no running interpreter".to_owned()))?;
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

// ---------------------------------------------------------------------------
// WarningsState — CPython's per-interpreter `WarningsState`.
// ---------------------------------------------------------------------------

struct WarnState {
    /// Last-known filters list (`WarningsState.filters`). Replaced by the
    /// live `warnings.filters` attribute whenever one is found.
    filters: Object,
    /// Last-known once-registry dict (`WarningsState.once_registry`).
    once_registry: Object,
    /// Last-known default action str (`WarningsState.default_action`).
    default_action: Object,
    /// Bumped by `_filters_mutated`; registries carry a `"version"` entry
    /// and are cleared when it goes stale.
    filters_version: i64,
}

// SAFETY of the static: `Object` is `Send + Sync` (`sync::Rc` is `Arc`,
// `RefCell` is the GIL-guarded cell), and every touch happens under the GIL.
static STATE: Mutex<Option<WarnState>> = Mutex::new(None);

/// The five default filters of a regular (non-debug, non-dev) build —
/// `init_filters` in `_warnings.c`. The module filter for the
/// `__main__` DeprecationWarning entry is a *plain string* (the C
/// module compares it with string equality), which
/// `test_default_filter_configuration` asserts verbatim.
fn default_filters() -> Vec<Object> {
    let bt = crate::builtin_types::builtin_types();
    let entry = |action: &'static str, category: &Rc<TypeObject>, module: Object| {
        Object::new_tuple(vec![
            Object::from_static(action),
            Object::None,
            Object::Type(category.clone()),
            module,
            Object::Int(0),
        ])
    };
    vec![
        entry(
            "default",
            &bt.deprecation_warning,
            Object::from_static("__main__"),
        ),
        entry("ignore", &bt.deprecation_warning, Object::None),
        entry("ignore", &bt.pending_deprecation_warning, Object::None),
        entry("ignore", &bt.import_warning, Object::None),
        entry("ignore", &bt.resource_warning, Object::None),
    ]
}

/// Run `f` against the (lazily initialised) state. Initialisation happens
/// once per process, so a re-import of `_warnings`
/// (`import_fresh_module('warnings', fresh=['_warnings'])`) re-exposes the
/// same state objects — CPython's static extension behaves the same way.
fn with_state<R>(f: impl FnOnce(&mut WarnState) -> R) -> R {
    let mut guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let st = guard.get_or_insert_with(|| WarnState {
        filters: Object::new_list(default_filters()),
        once_registry: Object::Dict(Rc::new(RefCell::new(DictData::default()))),
        default_action: Object::from_static("default"),
        filters_version: 0,
    });
    f(st)
}

// ---------------------------------------------------------------------------
// Module construction.
// ---------------------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_warnings"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "_warnings provides basic warning filtering support.\n\
                 It is a helper module to speed up interpreter start-up.",
            ),
        );
        let (filters, onceregistry, defaultaction) = with_state(|st| {
            (
                st.filters.clone(),
                st.once_registry.clone(),
                st.default_action.clone(),
            )
        });
        d.insert(DictKey(Object::from_static("filters")), filters);
        d.insert(DictKey(Object::from_static("_onceregistry")), onceregistry);
        d.insert(
            DictKey(Object::from_static("_defaultaction")),
            defaultaction,
        );
        d.insert(
            DictKey(Object::from_static("warn")),
            builtin_kw("warn", w_warn),
        );
        d.insert(
            DictKey(Object::from_static("warn_explicit")),
            builtin_kw("warn_explicit", w_warn_explicit),
        );
        d.insert(
            DictKey(Object::from_static("_filters_mutated")),
            builtin_kw("_filters_mutated", |_, _| {
                with_state(|st| st.filters_version += 1);
                Ok(Object::None)
            }),
        );
    }
    Rc::new(PyModule {
        name: "_warnings".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin_kw(
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

// ---------------------------------------------------------------------------
// get_warnings_attr — the live `warnings` module attribute, or None.
// ---------------------------------------------------------------------------

/// `sys.modules['warnings']` (importing it when `try_import`), then the
/// named attribute — `get_warnings_attr` in `_warnings.c`. Attribute
/// misses and import failures both come back as `None`; the caller falls
/// back to the internal state.
fn get_warnings_attr(attr: &str, try_import: bool) -> Option<Object> {
    let ip = interp().ok()?;
    let module = if try_import {
        ip.import_path("warnings").ok()?
    } else {
        ip.module_cache()
            .modules
            .borrow()
            .get(&StrKey("warnings"))
            .cloned()?
    };
    match &module {
        Object::Module(m) => m.dict.borrow().get(&StrKey(attr)).cloned(),
        other => ip.load_attr_public(other, attr).ok(),
    }
}

// ---------------------------------------------------------------------------
// Filter matching.
// ---------------------------------------------------------------------------

/// `check_matched`: `None` always matches, an exact `str` compares by
/// equality (the internal default filters are plain text), anything else
/// is assumed to be a compiled regex and dispatched to `.match(arg)`.
fn check_matched(obj: &Object, arg: &Object) -> Result<bool, RuntimeError> {
    match obj {
        Object::None => Ok(true),
        Object::Str(_) | Object::WStr(_) => member_eq(obj, arg),
        _ => {
            let ip = interp()?;
            let match_fn = ip.load_attr_public(obj, "match")?;
            let globals = ip.builtins_dict();
            let result = ip.call_object_with_globals(&match_fn, &[arg.clone()], &[], &globals)?;
            Ok(result.is_truthy())
        }
    }
}

fn issubclass_of(category: &Rc<TypeObject>, cat: &Object) -> Result<bool, RuntimeError> {
    match cat {
        Object::Type(k) => Ok(category.is_subclass_of(k)),
        Object::Tuple(items) => {
            for k in items.iter() {
                if issubclass_of(category, k)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(type_error(
            "issubclass() arg 2 must be a class, a tuple of classes, or a union",
        )),
    }
}

fn action_str(action: &Object) -> Option<String> {
    match action {
        Object::Str(_) | Object::WStr(_) => Some(action.to_str()),
        _ => None,
    }
}

/// `get_filter`: refresh the last-known filters list from the live
/// `warnings.filters` attribute, then scan for the first matching 5-tuple.
/// Returns the action plus the matched item (`None` for the default
/// action, used only in the "Unrecognized action" error).
fn get_filter(
    category: &Rc<TypeObject>,
    text: &Object,
    lineno: i64,
    module: &Object,
) -> Result<(Object, Object), RuntimeError> {
    if let Some(live) = get_warnings_attr("filters", false) {
        with_state(|st| st.filters = live);
    }
    let filters = with_state(|st| st.filters.clone());
    let Object::List(list) = &filters else {
        return Err(value_error("_warnings.filters must be a list"));
    };
    let mut i = 0usize;
    loop {
        // Re-borrow per iteration: a Python `.match()` call below can
        // re-enter and mutate the list (CPython walks it live too).
        let item = match list.borrow().get(i) {
            Some(it) => it.clone(),
            None => break,
        };
        let Object::Tuple(t) = &item else {
            return Err(value_error(format!(
                "_warnings.filters item {i} isn't a 5-tuple"
            )));
        };
        if t.len() != 5 {
            return Err(value_error(format!(
                "_warnings.filters item {i} isn't a 5-tuple"
            )));
        }
        let action = t[0].clone();
        if action_str(&action).is_none() {
            return Err(type_error(format!(
                "action must be a string, not '{}'",
                action.type_name_owned()
            )));
        }
        let good_msg = check_matched(&t[1], text)?;
        let good_mod = check_matched(&t[3], module)?;
        let is_subclass = issubclass_of(category, &t[2])?;
        let ln = t[4]
            .as_i64()
            .ok_or_else(|| type_error("filter lineno must be an int"))?;
        if good_msg && is_subclass && good_mod && (ln == 0 || lineno == ln) {
            return Ok((action, item));
        }
        i += 1;
    }
    // No filter matched: the default action, refreshed from the live
    // `warnings.defaultaction` when present.
    if let Some(live) = get_warnings_attr("defaultaction", false) {
        with_state(|st| st.default_action = live);
    }
    Ok((with_state(|st| st.default_action.clone()), Object::None))
}

/// `get_once_registry`: the live `warnings.onceregistry` (validated as a
/// dict) or the last-known internal one.
fn get_once_registry() -> Result<Object, RuntimeError> {
    if let Some(live) = get_warnings_attr("onceregistry", false) {
        if !matches!(live, Object::Dict(_)) {
            return Err(type_error(format!(
                "_warnings.onceregistry must be a dict, not '{}'",
                live.type_name_owned()
            )));
        }
        with_state(|st| st.once_registry = live.clone());
        return Ok(live);
    }
    Ok(with_state(|st| st.once_registry.clone()))
}

// ---------------------------------------------------------------------------
// Registry bookkeeping.
// ---------------------------------------------------------------------------

/// `already_warned`: version-check (clearing a stale registry), then look
/// up `key`; optionally record it. Returns true when the warning was
/// already issued.
fn already_warned(
    registry: &Rc<RefCell<DictData>>,
    key: &Object,
    should_set: bool,
) -> Result<bool, RuntimeError> {
    let version = with_state(|st| st.filters_version);
    let version_key = DictKey(Object::from_static("version"));
    let stale = match registry.borrow().get(&version_key) {
        Some(Object::Int(v)) => *v != version,
        _ => true,
    };
    if stale {
        let mut reg = registry.borrow_mut();
        reg.clear();
        reg.insert(version_key, Object::Int(version));
    } else if let Some(hit) = registry.borrow().get(&DictKey(key.clone())) {
        if hit.is_truthy() {
            return Ok(true);
        }
    }
    if should_set {
        registry
            .borrow_mut()
            .insert(DictKey(key.clone()), Object::Bool(true));
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Display.
// ---------------------------------------------------------------------------

/// The stderr fallback (`show_warning` in C), used only when the
/// `warnings` module (or its `_showwarnmsg`) is unavailable.
fn show_warning_fallback(
    ip: &mut Interp,
    filename: &Object,
    lineno: i64,
    text: &Object,
    category: &Rc<TypeObject>,
    sourceline: Option<&str>,
) {
    let text_str = ip.str_object(text).unwrap_or_else(|_| text.to_str());
    let mut out = format!(
        "{}:{lineno}: {}: {text_str}\n",
        filename.to_str(),
        category.name
    );
    if let Some(line) = sourceline {
        let stripped = line.trim_start_matches([' ', '\t', '\x0b', '\x0c']);
        out.push_str("  ");
        out.push_str(stripped);
        out.push('\n');
    }
    let stderr = {
        let modules = ip.module_cache().modules.borrow();
        match modules.get(&StrKey("sys")) {
            Some(Object::Module(m)) => m.dict.borrow().get(&StrKey("stderr")).cloned(),
            _ => None,
        }
    };
    let Some(stderr) = stderr else { return };
    if matches!(stderr, Object::None) {
        return;
    }
    let globals = ip.builtins_dict();
    if let Ok(write) = ip.load_attr_public(&stderr, "write") {
        let _ = ip.call_object_with_globals(&write, &[Object::from_str(out)], &[], &globals);
    }
}

/// `call_show_warning`: route through `warnings._showwarnmsg` with a
/// `warnings.WarningMessage` (file/line are always `None` on this path —
/// the Python hook re-derives them), falling back to plain stderr.
#[allow(clippy::too_many_arguments)]
fn call_show_warning(
    category: &Rc<TypeObject>,
    text: &Object,
    message: &Object,
    filename: &Object,
    lineno: i64,
    sourceline: Option<&str>,
    source: &Object,
) -> Result<(), RuntimeError> {
    let show_fn = get_warnings_attr("_showwarnmsg", true);
    let ip = interp()?;
    let Some(show_fn) = show_fn else {
        show_warning_fallback(ip, filename, lineno, text, category, sourceline);
        return Ok(());
    };
    if !is_callable(&show_fn) {
        return Err(type_error(
            "warnings._showwarnmsg() must be set to a callable",
        ));
    }
    let Some(warnmsg_cls) = get_warnings_attr("WarningMessage", false) else {
        return Err(runtime_error("unable to get warnings.WarningMessage"));
    };
    let globals = ip.builtins_dict();
    let msg = ip.call_object_with_globals(
        &warnmsg_cls,
        &[
            message.clone(),
            Object::Type(category.clone()),
            filename.clone(),
            Object::Int(lineno),
            Object::None,
            Object::None,
            source.clone(),
        ],
        &[],
        &globals,
    )?;
    let shown = ip.call_object_with_globals(&show_fn, &[msg.clone()], &[], &globals);
    // CPython decrefs the transient `WarningMessage` the moment
    // `_showwarnmsg` returns; when the hook did not retain it (the stock
    // stderr writer), the message — and, through `source=`, the very
    // object whose finalizer emitted the warning — dies right here. A
    // plain Rust drop would leave the tracked message pinned by its own
    // GC handle until the next cyclic collection, keeping e.g. an
    // unclosed `SpooledTemporaryFile`'s buffered fd alive across tests
    // (test_tempfile.test_warnings_on_cleanup). The refcount guard
    // inside leaves a *recorded* message (a `catch_warnings(record=True)`
    // log holds it) untouched.
    ip.maybe_prompt_reap_replaced(msg);
    shown?;
    Ok(())
}

// ---------------------------------------------------------------------------
// warn_explicit — the core pipeline.
// ---------------------------------------------------------------------------

/// `normalize_module`: strip a trailing `.py`, empty → `<unknown>`.
/// Operates on the filename *object* so surrogate-carrying names
/// (`WStr`) survive intact.
fn normalize_module(filename: &Object) -> Object {
    if let Object::WStr(cps) = filename {
        if cps.is_empty() {
            return Object::from_static("<unknown>");
        }
        let suffix: [u32; 3] = ['.' as u32, 'p' as u32, 'y' as u32];
        if cps.len() >= 3 && cps[cps.len() - 3..] == suffix {
            return Object::WStr(Rc::from(&cps[..cps.len() - 3]));
        }
        return filename.clone();
    }
    let s = filename.to_str();
    if s.is_empty() {
        Object::from_static("<unknown>")
    } else if let Some(stem) = s.strip_suffix(".py") {
        Object::from_str(stem.to_owned())
    } else {
        filename.clone()
    }
}

fn is_warning_instance(obj: &Object) -> Option<Rc<TypeObject>> {
    if let Object::Instance(inst) = obj {
        let warning = crate::builtin_types::builtin_types().warning.clone();
        if inst.cls().is_subclass_of(&warning) {
            return Some(inst.cls());
        }
    }
    None
}

/// The C `warn_explicit`: normalise message/category, consult the
/// registry, pick a filter action, record, raise or show.
#[allow(clippy::too_many_arguments)]
fn warn_explicit_core(
    category: Object,
    message: Object,
    filename: &Object,
    lineno: i64,
    module: Option<Object>,
    registry: Object,
    sourceline: Option<&str>,
    source: Object,
) -> Result<Object, RuntimeError> {
    // A None module means "emitted late during shutdown, the warnings
    // machinery is gone" — safest to drop the warning.
    if matches!(module, Some(Object::None)) {
        return Ok(Object::None);
    }
    let registry_rc: Option<Rc<RefCell<DictData>>> = match &registry {
        Object::None => None,
        Object::Dict(d) => Some(d.clone()),
        _ => return Err(type_error("'registry' must be a dict or None")),
    };
    let module = match module {
        Some(m) => m,
        None => normalize_module(filename),
    };

    // Normalise: a Warning *instance* supplies both text (str(message))
    // and category (its class); otherwise the message is the text and a
    // fresh instance is built by calling the category.
    let (text, message, category_cls) = match is_warning_instance(&message) {
        Some(cls) => {
            let text = Object::from_str(interp()?.str_object(&message)?);
            (text, message, cls)
        }
        None => {
            let Object::Type(cls) = &category else {
                return Err(type_error(format!(
                    "category must be a Warning subclass, not '{}'",
                    category.type_name_owned()
                )));
            };
            let cls = cls.clone();
            let ip = interp()?;
            let globals = ip.builtins_dict();
            let instance =
                ip.call_object_with_globals(&category, &[message.clone()], &[], &globals)?;
            (message, instance, cls)
        }
    };

    let key = Object::new_tuple(vec![
        text.clone(),
        Object::Type(category_cls.clone()),
        Object::Int(lineno),
    ]);
    if let Some(reg) = &registry_rc {
        if already_warned(reg, &key, false)? {
            return Ok(Object::None);
        }
    }

    let (action_obj, item) = get_filter(&category_cls, &text, lineno, &module)?;
    let action = action_str(&action_obj).unwrap_or_default();

    if action == "error" {
        return Err(RuntimeError::PyException(PyException::new(message)));
    }
    if action == "ignore" {
        return Ok(Object::None);
    }

    // Record, *except* for "always"/"all".
    let mut suppressed = false;
    if action != "always" && action != "all" {
        if let Some(reg) = &registry_rc {
            reg.borrow_mut()
                .insert(DictKey(key.clone()), Object::Bool(true));
        }
        if action == "once" {
            // With a caller registry, "once" dedupes in *it* under the
            // (text, category) altkey; otherwise in the global
            // once-registry (CPython's `get_once_registry`).
            let once_rc = match &registry_rc {
                Some(reg) => reg.clone(),
                None => {
                    let Object::Dict(d) = get_once_registry()? else {
                        return Err(type_error("_warnings.onceregistry must be a dict"));
                    };
                    d
                }
            };
            let altkey = Object::new_tuple(vec![text.clone(), Object::Type(category_cls.clone())]);
            suppressed = already_warned(&once_rc, &altkey, true)?;
        } else if action == "module" {
            if let Some(reg) = &registry_rc {
                let altkey = Object::new_tuple(vec![
                    text.clone(),
                    Object::Type(category_cls.clone()),
                    Object::Int(0),
                ]);
                suppressed = already_warned(reg, &altkey, true)?;
            }
        } else if action != "default" {
            let ip = interp()?;
            let action_repr = ip.repr_object(&action_obj)?;
            let item_repr = ip.repr_object(&item)?;
            return Err(runtime_error(format!(
                "Unrecognized action ({action_repr}) in warnings.filters:\n {item_repr}"
            )));
        }
    }

    if !suppressed {
        call_show_warning(
            &category_cls,
            &text,
            &message,
            filename,
            lineno,
            sourceline,
            &source,
        )?;
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// gh-86298 — module_globals → loader → source line.
// ---------------------------------------------------------------------------

/// `importlib._bootstrap_external._bless_my_loader` (gh-97850): resolve
/// the loader out of a `module_globals` dict, deprecating the legacy
/// `__loader__`-only shapes. `Ok(None)` means "no loader, no source" with
/// no error.
fn bless_my_loader(mg: &Rc<RefCell<DictData>>) -> Result<Option<Object>, RuntimeError> {
    let loader = mg
        .borrow()
        .get(&StrKey("__loader__"))
        .cloned()
        .unwrap_or(Object::None);
    let spec = mg.borrow().get(&StrKey("__spec__")).cloned();

    if matches!(loader, Object::None) {
        match &spec {
            None => return Ok(None),
            Some(Object::None) => {
                return Err(value_error("Module globals is missing a __spec__.loader"))
            }
            Some(_) => {}
        }
    }

    // getattr(spec, 'loader', missing): None distinguishes "attribute is
    // None" from "attribute missing" via the Option.
    let spec_loader: Option<Object> = match &spec {
        None | Some(Object::None) => None,
        Some(s) => interp()?.load_attr_public(s, "loader").ok(),
    };
    let spec_loader_missing = spec_loader.is_none();
    let spec_loader = spec_loader.filter(|l| !matches!(l, Object::None));

    let deprecate = |msg: &str| -> Result<(), RuntimeError> {
        let cls = crate::builtin_types::builtin_types()
            .deprecation_warning
            .clone();
        warn_with_context(
            Object::from_str(msg.to_owned()),
            Object::Type(cls),
            1,
            Object::None,
            &[],
        )
        .map(|_| ())
    };

    let spec_loader = match spec_loader {
        Some(l) => l,
        None => {
            if matches!(loader, Object::None) {
                let msg = "Module globals is missing a __spec__.loader";
                return Err(
                    if spec_loader_missing && !matches!(spec, Some(Object::None)) {
                        RuntimeError::PyException(PyException::new(
                            crate::builtin_types::make_exception_with_class(
                                crate::builtin_types::builtin_types()
                                    .by_name("AttributeError")
                                    .expect("AttributeError exists"),
                                msg,
                            ),
                        ))
                    } else {
                        value_error(msg)
                    },
                );
            }
            deprecate("Module globals is missing a __spec__.loader")?;
            loader.clone()
        }
    };

    if !matches!(loader, Object::None) && !member_eq(&loader, &spec_loader)? {
        deprecate("Module globals; __loader__ != __spec__.loader")?;
        return Ok(Some(loader));
    }
    Ok(Some(spec_loader))
}

/// `get_source_line`: bless the loader, call its optional
/// `get_source(name)`, split and pick line `lineno`. Shape problems
/// (bad `splitlines` result, out-of-range line) are swallowed — only the
/// bless errors propagate (bpo-31285 / gh-86298).
fn get_source_line(
    mg: &Rc<RefCell<DictData>>,
    lineno: i64,
) -> Result<Option<String>, RuntimeError> {
    let Some(loader) = bless_my_loader(mg)? else {
        return Ok(None);
    };
    let Some(module_name) = mg.borrow().get(&StrKey("__name__")).cloned() else {
        return Ok(None);
    };
    let ip = interp()?;
    let Ok(get_source) = ip.load_attr_public(&loader, "get_source") else {
        return Ok(None);
    };
    let globals = ip.builtins_dict();
    let source = ip.call_object_with_globals(&get_source, &[module_name], &[], &globals)?;
    if matches!(source, Object::None) {
        return Ok(None);
    }
    let Ok(splitlines) = ip.load_attr_public(&source, "splitlines") else {
        return Ok(None);
    };
    let Ok(lines) = ip.call_object_with_globals(&splitlines, &[], &[], &globals) else {
        return Ok(None);
    };
    let Object::List(lines) = &lines else {
        return Ok(None);
    };
    if lineno < 1 {
        return Ok(None);
    }
    let line = lines.borrow().get(lineno as usize - 1).cloned();
    Ok(line.map(|l| l.to_str()))
}

// ---------------------------------------------------------------------------
// warn — caller-context discovery.
// ---------------------------------------------------------------------------

/// `is_internal_frame`: importlib machinery frames are skipped when
/// resolving `stacklevel`.
fn is_internal_filename(filename: &str) -> bool {
    filename.contains("importlib") && filename.contains("_bootstrap")
}

/// `setup_context`: walk the caller's frame stack per `stacklevel`,
/// returning (filename, lineno, module-name object, registry) — the
/// registry being the frame globals' `__warningregistry__`, created on
/// demand. With no frame left (late shutdown), CPython 3.13 attributes
/// the warning to `<sys>:0` against the `sys` module dict.
fn setup_context(
    stacklevel: i64,
    skip_file_prefixes: &[String],
) -> Result<(String, i64, Object, Object), RuntimeError> {
    // Per-thread frame stack, with the interpreter's own as a fallback
    // (shutdown finalizers run `__del__` without re-activating handles —
    // same fallback `sys._getframe` keeps).
    let frames: Option<crate::object::FrameStack> =
        match crate::vm_singletons::current_thread_handles() {
            Some(h) => Some(h.frame_stack.clone()),
            None => interp().ok().map(|ip| ip.frame_stack.clone()),
        };
    // The walk only needs filename / lineno / globals, all present on
    // the cheap shells — no `PyFrame` materialisation (RFC 0058).
    let frame: Option<Rc<crate::object::FrameShell>> = frames.and_then(|fs| {
        let stack = fs.borrow();
        if stack.is_empty() {
            return None;
        }
        let is_internal =
            |f: &Rc<crate::object::FrameShell>| is_internal_filename(&f.code.filename);
        let to_skip = |f: &Rc<crate::object::FrameShell>| {
            is_internal(f)
                || skip_file_prefixes
                    .iter()
                    .any(|p| f.code.filename.starts_with(p.as_str()))
        };
        let mut idx: isize = stack.len() as isize - 1;
        let mut level = stacklevel;
        if level <= 0 || is_internal(&stack[idx as usize]) {
            while level > 1 && idx >= 0 {
                idx -= 1;
                level -= 1;
            }
        } else {
            while level > 1 && idx >= 0 {
                // next_external_frame: hop to the next non-internal,
                // non-skipped frame.
                loop {
                    idx -= 1;
                    if idx < 0 || !to_skip(&stack[idx as usize]) {
                        break;
                    }
                }
                level -= 1;
            }
        }
        if idx < 0 {
            None
        } else {
            Some(stack[idx as usize].clone())
        }
    });

    let (globals, filename, lineno) = match frame {
        Some(f) => {
            let filename = f.code.filename.clone();
            let lineno = i64::from(f.current_lineno());
            (f.globals.clone(), filename, lineno)
        }
        None => {
            // Late-shutdown / no-frame warning: attributed to `<sys>:0`,
            // registry parked in the sys module dict.
            let ip = interp()?;
            let sysdict = {
                let modules = ip.module_cache().modules.borrow();
                match modules.get(&StrKey("sys")) {
                    Some(Object::Module(m)) => m.dict.clone(),
                    _ => Rc::new(RefCell::new(DictData::default())),
                }
            };
            (sysdict, "<sys>".to_owned(), 0)
        }
    };

    let registry_key = DictKey(Object::from_static("__warningregistry__"));
    let registry = match globals.borrow().get(&registry_key) {
        Some(r) => Some(r.clone()),
        None => None,
    };
    let registry = match registry {
        Some(r) => r,
        None => {
            let fresh = Object::Dict(Rc::new(RefCell::new(DictData::default())));
            globals.borrow_mut().insert(registry_key, fresh.clone());
            fresh
        }
    };
    let module = match globals.borrow().get(&StrKey("__name__")) {
        Some(Object::None) | None => Object::from_static("<string>"),
        Some(m) => m.clone(),
    };
    Ok((filename, lineno, module, registry))
}

/// `get_category`: a Warning instance dictates its own class; None means
/// UserWarning; anything else must be a Warning subclass.
fn get_category(message: &Object, category: &Object) -> Result<Rc<TypeObject>, RuntimeError> {
    if let Some(cls) = is_warning_instance(message) {
        return Ok(cls);
    }
    let bt = crate::builtin_types::builtin_types();
    let cls = match category {
        Object::None => bt.user_warning.clone(),
        Object::Type(t) => t.clone(),
        other => {
            return Err(type_error(format!(
                "category must be a Warning subclass, not '{}'",
                other.type_name_owned()
            )))
        }
    };
    if !cls.is_subclass_of(&bt.warning) {
        return Err(type_error(format!(
            "category must be a Warning subclass, not '{}'",
            cls.name
        )));
    }
    Ok(cls)
}

/// `do_warn`: the shared body of `warn()` (and the bless deprecations).
fn warn_with_context(
    message: Object,
    category: Object,
    stacklevel: i64,
    source: Object,
    skip_file_prefixes: &[String],
) -> Result<Object, RuntimeError> {
    let category_cls = get_category(&message, &category)?;
    let (filename, lineno, module, registry) = setup_context(stacklevel, skip_file_prefixes)?;
    warn_explicit_core(
        Object::Type(category_cls),
        message,
        &Object::from_str(filename),
        lineno,
        Some(module),
        registry,
        None,
        source,
    )
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

fn arg_or_kw<'a>(
    args: &'a [Object],
    kwargs: &'a [(String, Object)],
    pos: usize,
    name: &str,
) -> Option<&'a Object> {
    if let Some(v) = args.get(pos) {
        return Some(v);
    }
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// `warn(message, category=None, stacklevel=1, source=None, *,
/// skip_file_prefixes=())`.
fn w_warn(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let message = arg_or_kw(args, kwargs, 0, "message")
        .cloned()
        .ok_or_else(|| type_error("warn() missing required argument: 'message' (pos 1)"))?;
    let category = arg_or_kw(args, kwargs, 1, "category")
        .cloned()
        .unwrap_or(Object::None);
    let stacklevel = match arg_or_kw(args, kwargs, 2, "stacklevel") {
        None => 1,
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("'stacklevel' must be an integer"))?,
    };
    let source = arg_or_kw(args, kwargs, 3, "source")
        .cloned()
        .unwrap_or(Object::None);
    let mut prefixes: Vec<String> = Vec::new();
    let mut stacklevel = stacklevel;
    if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == "skip_file_prefixes") {
        let Object::Tuple(items) = v else {
            return Err(type_error(format!(
                "warn() argument 'skip_file_prefixes' must be a tuple, not {}",
                v.type_name_owned()
            )));
        };
        for it in items.iter() {
            match it {
                Object::Str(_) | Object::WStr(_) => prefixes.push(it.to_str()),
                _ => {
                    return Err(type_error(
                        "warn() argument 'skip_file_prefixes' must be a tuple of strs",
                    ))
                }
            }
        }
        // A non-empty prefix set means "attribute the warning to code
        // outside these files" — never the immediate caller.
        if !prefixes.is_empty() && stacklevel < 2 {
            stacklevel = 2;
        }
    }
    warn_with_context(message, category, stacklevel, source, &prefixes)
}

/// `warn_explicit(message, category, filename, lineno, module=None,
/// registry=None, module_globals=None, source=None)`.
fn w_warn_explicit(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let message = arg_or_kw(args, kwargs, 0, "message")
        .cloned()
        .ok_or_else(|| {
            type_error("warn_explicit() missing required argument: 'message' (pos 1)")
        })?;
    let category = arg_or_kw(args, kwargs, 1, "category")
        .cloned()
        .ok_or_else(|| {
            type_error("warn_explicit() missing required argument: 'category' (pos 2)")
        })?;
    let filename = match arg_or_kw(args, kwargs, 2, "filename") {
        Some(f @ (Object::Str(_) | Object::WStr(_))) => f.clone(),
        Some(other) => {
            return Err(type_error(format!(
                "warn_explicit() argument 'filename' must be str, not {}",
                other.type_name_owned()
            )))
        }
        None => {
            return Err(type_error(
                "warn_explicit() missing required argument: 'filename' (pos 3)",
            ))
        }
    };
    let lineno = match arg_or_kw(args, kwargs, 3, "lineno") {
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("'lineno' must be an integer"))?,
        None => {
            return Err(type_error(
                "warn_explicit() missing required argument: 'lineno' (pos 4)",
            ))
        }
    };
    let module = arg_or_kw(args, kwargs, 4, "module").cloned();
    let registry = arg_or_kw(args, kwargs, 5, "registry")
        .cloned()
        .unwrap_or(Object::None);
    let module_globals = arg_or_kw(args, kwargs, 6, "module_globals")
        .cloned()
        .unwrap_or(Object::None);
    let source = arg_or_kw(args, kwargs, 7, "source")
        .cloned()
        .unwrap_or(Object::None);

    let sourceline: Option<String> = match &module_globals {
        Object::None => None,
        Object::Dict(mg) => get_source_line(mg, lineno)?,
        other => {
            return Err(type_error(format!(
                "module_globals must be a dict, not '{}'",
                other.type_name_owned()
            )))
        }
    };

    warn_explicit_core(
        category,
        message,
        &filename,
        lineno,
        module,
        registry,
        sourceline.as_deref(),
        source,
    )
}
