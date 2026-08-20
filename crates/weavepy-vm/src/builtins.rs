//! Built-in functions and per-type methods.
//!
//! Two responsibilities live here:
//!
//! 1. [`default_builtins`] returns the dict that lives behind every
//!    module's `__builtins__` — `print`, `len`, `range`, the
//!    type-coercion callables, and so on.
//! 2. [`lookup_method`] resolves an attribute access on a built-in
//!    type (`xs.append`, `s.upper`, `d.get`) to a `BuiltinFn`. The
//!    VM wraps that in a `BoundMethod` so the receiver flows through
//!    as the first arg on call.
//!
//! Builtins close over no state: each takes a `&[Object]` and returns
//! a `Result<Object, RuntimeError>`. Stateful builtins (notably
//! `print`, which needs the interpreter's stdout sink) are installed
//! by [`crate::Interpreter::install_print_into`].

use crate::sync::Rc;
use crate::sync::RefCell;

use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

use crate::builtin_types::{builtin_types, instance_is_subclass};
use crate::error::{
    index_error, key_error, key_error_object, runtime_error, stop_iteration, type_error,
    value_error, RuntimeError,
};
use crate::object::{BuiltinFn, DictData, DictKey, MethodWrapper, Object, PyIterator, Range};

/// Marker name on the `BuiltinFn` returned by [`build_class_builtin`].
/// The VM looks for this when dispatching `Call` so the call can be
/// routed through the interpreter (it needs to run the class body).
pub const BUILD_CLASS_NAME: &str = "__build_class__";

/// The `__build_class__` callable. The body always errors — the VM
/// recognises the name and runs its own class-construction path
/// before this is ever invoked. The placeholder is here so module
/// dis output reads naturally.
pub fn build_class_builtin() -> BuiltinFn {
    BuiltinFn {
        name: BUILD_CLASS_NAME,
        binds_instance: false,
        call: Box::new(|_args: &[Object]| {
            Err(runtime_error("internal: __build_class__ called outside VM"))
        }),
        call_kw: None,
    }
}

/// Resolve the native constructor function for a built-in *type* by name.
///
/// The VM's instantiation fallback (`builtin_constructor_for`) needs the
/// `b_*` constructor (e.g. `b_set`) even though the user-visible
/// `__builtins__` now maps these names to the real `type` objects. Keeping
/// this lookup independent of the `__builtins__` dict lets both coexist:
/// `builtins.set is set` (a type) while `set(...)` still constructs through
/// the native helper.
pub(crate) fn builtin_type_constructor(name: &str) -> Option<Rc<BuiltinFn>> {
    macro_rules! ctor {
        ($n:literal, $body:expr) => {
            Some(Rc::new(BuiltinFn {
                name: $n,
                binds_instance: false,
                call: Box::new($body),
                call_kw: None,
            }))
        };
        ($n:literal, $body:expr, $kw:expr) => {
            Some(Rc::new(BuiltinFn {
                name: $n,
                binds_instance: false,
                call: Box::new($body),
                call_kw: Some(Box::new($kw)),
            }))
        };
    }
    match name {
        "str" => ctor!("str", b_str, b_str_kw),
        "int" => ctor!("int", b_int),
        "float" => ctor!("float", b_float),
        "complex" => ctor!("complex", b_complex),
        "bool" => ctor!("bool", b_bool),
        "list" => ctor!("list", b_list),
        "tuple" => ctor!("tuple", b_tuple),
        "dict" => ctor!("dict", b_dict),
        "set" => ctor!("set", b_set),
        "frozenset" => ctor!("frozenset", b_frozenset),
        "bytes" => ctor!("bytes", b_bytes, b_bytes_kw),
        "bytearray" => ctor!("bytearray", b_bytearray, b_bytearray_kw),
        "object" => ctor!("object", b_object),
        "type" => ctor!("type", b_type),
        "range" => ctor!("range", b_range),
        "slice" => ctor!("slice", b_slice),
        "memoryview" => ctor!("memoryview", b_memoryview),
        "enumerate" => ctor!("enumerate", b_enumerate, b_enumerate_kw),
        "reversed" => ctor!("reversed", b_reversed),
        "cell" => ctor!("cell", b_cell),
        _ => None,
    }
}

/// `types.CellType()` / `types.CellType(value)` — a real closure cell
/// (CPython `cell_new`), empty when constructed without a value.
pub(crate) fn b_cell(args: &[Object]) -> Result<Object, RuntimeError> {
    let contents = match args {
        [] => Object::Unbound,
        [v] => v.clone(),
        n => {
            return Err(type_error(format!(
                "cell expected at most 1 argument, got {}",
                n.len()
            )));
        }
    };
    Ok(Object::Cell(Rc::new(crate::sync::RefCell::new(contents))))
}

/// `slice(stop)` / `slice(start, stop[, step])` → a real `Object::Slice`,
/// the same representation the `BUILD_SLICE` opcode produces for `a:b:c`.
/// Without this the type's generic instantiation path made a bare
/// `object` instance that the subscript handlers (which match
/// `Object::Slice`) rejected. Missing positions default to `None`,
/// matching CPython's `slice` type.
pub(crate) fn b_slice(args: &[Object]) -> Result<Object, RuntimeError> {
    let (start, stop, step) = match args.len() {
        0 => {
            return Err(type_error("slice expected at least 1 argument, got 0"));
        }
        1 => (Object::None, args[0].clone(), Object::None),
        2 => (args[0].clone(), args[1].clone(), Object::None),
        3 => (args[0].clone(), args[1].clone(), args[2].clone()),
        n => {
            return Err(type_error(format!(
                "slice expected at most 3 arguments, got {n}"
            )));
        }
    };
    Ok(Object::Slice(Rc::new(crate::object::PySlice {
        start,
        stop,
        step,
    })))
}

/// Build the dict that backs the `builtins` module.
pub fn default_builtins() -> DictData {
    let mut d = DictData::default();
    macro_rules! reg {
        ($name:literal, $body:expr) => {{
            let f = BuiltinFn {
                name: $name,
                binds_instance: false,
                call: Box::new($body),
                call_kw: None,
            };
            d.insert(
                DictKey(Object::from_static($name)),
                Object::Builtin(Rc::new(f)),
            );
        }};
    }
    // As `reg!`, but the body also receives the keyword-argument list (for
    // builtins with documented named parameters, e.g. `enumerate(x, start=)`).
    macro_rules! reg_kw {
        ($name:literal, $body:expr) => {{
            let f = BuiltinFn {
                name: $name,
                binds_instance: false,
                call: Box::new(|args| $body(args, &[])),
                call_kw: Some(Box::new($body)),
            };
            d.insert(
                DictKey(Object::from_static($name)),
                Object::Builtin(Rc::new(f)),
            );
        }};
    }

    // The VM intercepts calls to this marker (`LoadBuildClass` /
    // `dispatch_call`); it lives in the dict so sandbox copies of the
    // builtins namespace (`frozendict(builtins.__dict__)`) keep class
    // statements working (test_builtin test_exec_globals).
    d.insert(
        DictKey(Object::from_static("__build_class__")),
        Object::Builtin(Rc::new(build_class_builtin())),
    );
    reg!("len", b_len);
    reg!("range", b_range);
    reg_kw!("str", b_str_kw);
    reg!("repr", b_repr);
    reg!("int", b_int);
    reg!("float", b_float);
    reg!("complex", b_complex);
    reg!("bool", b_bool);
    reg!("list", b_list);
    reg!("tuple", b_tuple);
    reg!("dict", b_dict);
    reg!("set", b_set);
    reg!("frozenset", b_frozenset);
    reg!("bytes", b_bytes);
    reg!("bytearray", b_bytearray);
    // `open` accepts keyword arguments (`encoding`, `errors`,
    // `newline`, `buffering`, `closefd`, `opener`), so wire it through
    // the kwargs-aware constructor — we silently fold known kwargs
    // into positional slots and ignore the unimplemented ones (they
    // mostly affect encoding handling, which we already do by default).
    {
        let f = BuiltinFn {
            name: "open",
            binds_instance: false,
            call: Box::new(b_open),
            call_kw: Some(Box::new(b_open_kw)),
        };
        d.insert(
            DictKey(Object::from_static("open")),
            Object::Builtin(Rc::new(f)),
        );
    }
    reg!("type", b_type);
    reg!("abs", b_abs);
    reg!("min", b_min);
    reg!("max", b_max);
    reg!("sum", b_sum);
    reg!("sorted", b_sorted);
    reg!("reversed", b_reversed);
    reg_kw!("enumerate", b_enumerate_kw);
    reg!("zip", b_zip);
    reg!("map", b_map);
    reg!("filter", b_filter);
    reg!("all", b_all);
    reg!("any", b_any);
    reg!("isinstance", b_isinstance);
    reg!("issubclass", b_issubclass);
    reg!("super", b_super);
    reg!("id", b_id);
    reg!("hash", b_hash);
    reg!("dir", b_dir);
    reg!("hex", b_hex);
    reg!("oct", b_oct);
    reg!("bin", b_bin);
    reg!("chr", b_chr);
    reg!("ord", b_ord);
    reg!("input", b_input_unsupported);
    reg!("next", b_next);
    reg!("iter", b_iter);
    reg!("aiter", b_aiter);
    reg!("anext", b_anext);
    reg!(
        "_weavepy_mark_iterable_coroutine",
        b_mark_iterable_coroutine
    );
    reg!("divmod", b_divmod);
    reg_kw!("round", b_round_kw);
    reg!("format", b_format);
    reg!("ascii", b_ascii);
    // `property`, `staticmethod`, `classmethod` are exposed as
    // *types* now (see [`crate::builtin_types::BuiltinTypes`]),
    // not as bare functions. The corresponding constructors are
    // wired through [`crate::Vm::builtin_constructor_for`].
    reg!("getattr", b_getattr);
    reg!("setattr", b_setattr);
    reg!("delattr", b_delattr);
    reg!("hasattr", b_hasattr);
    reg!("vars", b_vars);
    reg!("callable", b_callable);
    reg!("object", b_object);
    reg!("globals", b_globals);
    reg!("locals", b_locals);
    // RFC 0023 — the long-tail builtins that scripts routinely
    // reach for. `breakpoint` is intercepted by the VM so it can
    // honour `sys.breakpointhook`; `help`/`copyright`/`license` are
    // intentionally cheap "interactive use only" stubs.
    reg_kw!("pow", b_pow_kw);
    reg!("breakpoint", b_breakpoint);
    reg!("memoryview", b_memoryview);
    {
        let f = BuiltinFn {
            name: "__vm:input",
            binds_instance: false,
            call: Box::new(b_input_unsupported),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("input")),
            Object::Builtin(Rc::new(f)),
        );
    }
    d.insert(
        DictKey(Object::from_static("help")),
        crate::vm_singletons::interactive_printer(
            "help",
            "Type help() for interactive help, or help(object) for help about object.",
        ),
    );
    d.insert(
        DictKey(Object::from_static("copyright")),
        crate::vm_singletons::interactive_printer(
            "copyright",
            "Copyright (c) 2026 The WeavePy Authors.\nAll Rights Reserved.\n\nWeavePy is dual-licensed under MIT OR Apache-2.0.",
        ),
    );
    d.insert(
        DictKey(Object::from_static("license")),
        crate::vm_singletons::interactive_printer(
            "license",
            "Type license() to see the full license text.\nWeavePy is licensed under MIT OR Apache-2.0.",
        ),
    );
    d.insert(
        DictKey(Object::from_static("credits")),
        crate::vm_singletons::interactive_printer(
            "credits",
            "Thanks to the CPython, Rust, PyPy, and rustls communities for paving the way.",
        ),
    );
    d.insert(
        DictKey(Object::from_static("quit")),
        crate::vm_singletons::quitter("quit"),
    );
    d.insert(
        DictKey(Object::from_static("exit")),
        crate::vm_singletons::quitter("exit"),
    );
    // `__import__`, `compile`, `exec`, `eval` are VM intrinsics: the
    // registered closures are only placeholders, the VM intercepts
    // calls to builtins whose internal name carries the `__vm:`
    // prefix and runs the real implementation, which needs access to
    // the interpreter state. We use a sentinel prefix on the
    // `BuiltinFn::name` field so user modules that re-export their
    // own `compile`/`exec`/`eval` (e.g. the `re` module's
    // `re.compile`) don't get hijacked by the global intrinsic
    // dispatcher.
    {
        let f = BuiltinFn {
            name: "__vm:__import__",
            binds_instance: false,
            call: Box::new(b_import_placeholder),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("__import__")),
            Object::Builtin(Rc::new(f)),
        );
    }
    {
        let f = BuiltinFn {
            name: "__vm:compile",
            binds_instance: false,
            call: Box::new(b_vm_intrinsic),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("compile")),
            Object::Builtin(Rc::new(f)),
        );
    }
    {
        let f = BuiltinFn {
            name: "__vm:exec",
            binds_instance: false,
            call: Box::new(b_vm_intrinsic),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("exec")),
            Object::Builtin(Rc::new(f)),
        );
    }
    {
        let f = BuiltinFn {
            name: "__vm:eval",
            binds_instance: false,
            call: Box::new(b_vm_intrinsic),
            call_kw: None,
        };
        d.insert(
            DictKey(Object::from_static("eval")),
            Object::Builtin(Rc::new(f)),
        );
    }

    // CPython exposes two singletons in `builtins`: `NotImplemented`
    // (the rich-comparison fallback sentinel) and `Ellipsis` (the
    // value bound by `...`). We model both as fresh `object()`
    // instances created once at registry build time so identity
    // tests (`a is NotImplemented`) work as expected.
    d.insert(
        DictKey(Object::from_static("NotImplemented")),
        crate::vm_singletons::not_implemented(),
    );
    d.insert(
        DictKey(Object::from_static("Ellipsis")),
        crate::vm_singletons::ellipsis(),
    );
    // `__debug__` is a compile-time constant builtin: `True` for a normal
    // interpreter, `False` only under `-O`. WeavePy has no `-O` mode yet,
    // so it is unconditionally `True` (matching `python3` with no flags).
    // Used by `assert` lowering and reached directly by test_exceptions.
    d.insert(
        DictKey(Object::from_static("__debug__")),
        Object::Bool(true),
    );

    // RFC 0026 — the shared `builtins` dict needs to mirror every
    // *exception* type that `builtin_types().as_globals()` injects
    // into per-module globals. Without this, code that runs in an
    // "outside" globals dict (for example via `exec()` from runpy or
    // `concurrent.futures` workers) can't see `Exception`,
    // `TypeError`, …. We *only* re-add exception classes: the data
    // types (`int`, `set`, `list`, …) already have function-flavoured
    // entries registered above which the VM routes through its
    // specialised constructors, and overwriting those with the bare
    // `Object::Type` would break `set()` / `list()` instantiation.
    for (n, value) in crate::builtin_types::builtin_types().as_globals() {
        if !is_exception_like(&n) {
            continue;
        }
        d.insert(DictKey(Object::from_str(n)), value);
    }

    d
}

/// True for every CPython built-in name that exists in the `builtins`
/// dict as a class-shaped object (every concrete exception type and the
/// `Warning` hierarchy). Used to filter `builtin_types().as_globals()`
/// down to entries that don't conflict with the function-flavoured
/// `int`/`set`/`list` entries already registered.
fn is_exception_like(name: &str) -> bool {
    name.ends_with("Error")
        || name.ends_with("Warning")
        || name.ends_with("Exception")
        || matches!(
            name,
            "BaseException"
                | "Exception"
                | "GeneratorExit"
                | "KeyboardInterrupt"
                | "SystemExit"
                | "StopIteration"
                | "StopAsyncIteration"
                | "BaseExceptionGroup"
                | "ExceptionGroup"
                | "NotImplemented"
                | "Ellipsis"
        )
}

// ---------- method dispatch ----------

/// Resolve `obj.<name>` to a callable, or `None` if there's no such
/// method. The returned [`Object`] is always a [`Object::Builtin`];
/// the VM wraps it as a [`crate::object::BoundMethod`] so the
/// receiver flows through as the first argument on call.
pub fn lookup_method(obj: &Object, name: &str) -> Option<Object> {
    let f: Option<BuiltinFn> = match obj {
        Object::Str(_) => match name {
            "upper" => Some(method("upper", str_upper)),
            "lower" => Some(method("lower", str_lower)),
            "title" => Some(method("title", str_title)),
            "capitalize" => Some(method("capitalize", str_capitalize)),
            "casefold" => Some(method("casefold", str_casefold)),
            "swapcase" => Some(method("swapcase", str_swapcase)),
            "strip" => Some(method("strip", str_strip)),
            "lstrip" => Some(method("lstrip", str_lstrip)),
            "rstrip" => Some(method("rstrip", str_rstrip)),
            "split" => Some(method_kw("split", str_split)),
            "rsplit" => Some(method_kw("rsplit", str_rsplit)),
            "splitlines" => Some(method_kw("splitlines", str_splitlines)),
            "join" => Some(method("join", str_join)),
            "startswith" => Some(method("startswith", str_startswith)),
            "endswith" => Some(method("endswith", str_endswith)),
            "replace" => Some(method_kw("replace", str_replace_kw)),
            "find" => Some(method("find", str_find)),
            "rfind" => Some(method("rfind", str_rfind)),
            "index" => Some(method("index", str_index)),
            "rindex" => Some(method("rindex", str_rindex)),
            "count" => Some(method("count", str_count)),
            "partition" => Some(method("partition", str_partition)),
            "rpartition" => Some(method("rpartition", str_rpartition)),
            "isdigit" => Some(method("isdigit", str_isdigit)),
            "isalpha" => Some(method("isalpha", str_isalpha)),
            "isalnum" => Some(method("isalnum", str_isalnum)),
            "isspace" => Some(method("isspace", str_isspace)),
            "isupper" => Some(method("isupper", str_isupper)),
            "islower" => Some(method("islower", str_islower)),
            "istitle" => Some(method("istitle", str_istitle)),
            "isascii" => Some(method("isascii", str_isascii)),
            "isnumeric" => Some(method("isnumeric", str_isnumeric)),
            "isdecimal" => Some(method("isdecimal", str_isdecimal)),
            "isidentifier" => Some(method("isidentifier", str_isidentifier)),
            "isprintable" => Some(method("isprintable", str_isprintable)),
            "zfill" => Some(method("zfill", str_zfill)),
            "ljust" => Some(method("ljust", str_ljust)),
            "rjust" => Some(method("rjust", str_rjust)),
            "center" => Some(method("center", str_center)),
            "expandtabs" => Some(method_kw("expandtabs", str_expandtabs_kw)),
            "encode" => Some(method_kw("encode", str_encode)),
            "removeprefix" => Some(method("removeprefix", str_removeprefix)),
            "removesuffix" => Some(method("removesuffix", str_removesuffix)),
            "format" => Some(method_kw(".format", str_format_kw)),
            "format_map" => Some(method(".format_map", str_format_map)),
            "translate" => Some(method("translate", str_translate)),
            "maketrans" => Some(static_method("maketrans", str_maketrans)),
            // Sequence dunders so `hasattr(s, '__getitem__')` and direct
            // `str.__getitem__(s, i)` calls work (CPython exposes these as
            // slot wrappers; `operator.concat` probes `__getitem__`).
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__add__" => Some(seq_dunder_binop(
                "__add__",
                weavepy_compiler::BinOpKind::Add,
                false,
            )),
            "__mul__" => Some(seq_dunder_binop(
                "__mul__",
                weavepy_compiler::BinOpKind::Mult,
                false,
            )),
            "__rmul__" => Some(seq_dunder_binop(
                "__rmul__",
                weavepy_compiler::BinOpKind::Mult,
                true,
            )),
            "__mod__" => Some(seq_dunder_binop(
                "__mod__",
                weavepy_compiler::BinOpKind::Mod,
                false,
            )),
            "__rmod__" => Some(method("__rmod__", str_dunder_rmod)),
            _ => None,
        },
        Object::List(_) => match name {
            "append" => Some(method("append", list_append)),
            "pop" => Some(method("pop", list_pop)),
            "extend" => Some(method("extend", list_extend)),
            "insert" => Some(method("insert", list_insert)),
            "remove" => Some(method("remove", list_remove)),
            "index" => Some(method("index", list_index)),
            "count" => Some(method("count", list_count)),
            "sort" => Some(method("sort", list_sort)),
            "reverse" => Some(method("reverse", list_reverse)),
            "clear" => Some(method("clear", list_clear)),
            "copy" => Some(method("copy", list_copy)),
            // Dunders so `list.__setitem__` / `super().__getitem__` resolve
            // for `list` subclasses (`class C(list)`).
            "__getitem__" => Some(method("__getitem__", list_getitem)),
            "__setitem__" => Some(method("__setitem__", list_setitem)),
            "__delitem__" => Some(method("__delitem__", list_delitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__add__" => Some(seq_dunder_binop(
                "__add__",
                weavepy_compiler::BinOpKind::Add,
                false,
            )),
            "__mul__" => Some(seq_dunder_binop(
                "__mul__",
                weavepy_compiler::BinOpKind::Mult,
                false,
            )),
            "__rmul__" => Some(seq_dunder_binop(
                "__rmul__",
                weavepy_compiler::BinOpKind::Mult,
                true,
            )),
            "__iadd__" => Some(method("__iadd__", list_iadd)),
            "__imul__" => Some(method("__imul__", list_imul)),
            _ => None,
        },
        // `range` is a genuine immutable sequence: CPython exposes `.index`
        // and `.count` on it (pandas' `RangeIndex.get_loc` calls
        // `self._range.index(int(key))` to locate a label).
        Object::Range(_) => match name {
            "index" => Some(method("index", range_index)),
            "count" => Some(method("count", range_count)),
            // Slot wrapper: `range(1, 20).__getitem__(i)` with full
            // `__index__` + slice support (test_index.RangeTestCase).
            "__getitem__" => Some(method("__getitem__", range_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            // `range.__bool__` / `range.__reversed__` are real `tp_dict`
            // entries in CPython (RFC 0056 WS4).
            "__bool__" => Some(method("__bool__", |args| {
                let recv = args
                    .first()
                    .ok_or_else(|| type_error("__bool__() missing self"))?;
                Ok(Object::Bool(recv.len()? > 0))
            })),
            "__reversed__" => Some(method("__reversed__", |args| {
                b_reversed(std::slice::from_ref(
                    args.first()
                        .ok_or_else(|| type_error("__reversed__() missing self"))?,
                ))
            })),
            _ => None,
        },
        Object::Dict(_) => match name {
            "get" => Some(method("get", dict_get)),
            "keys" => Some(method("keys", dict_keys)),
            "values" => Some(method("values", dict_values)),
            "items" => Some(method("items", dict_items)),
            "pop" => Some(method("pop", dict_pop)),
            "update" => Some(method("update", dict_update)),
            "clear" => Some(method("clear", dict_clear)),
            "setdefault" => Some(method("setdefault", dict_setdefault)),
            "copy" => Some(method("copy", dict_copy)),
            // A classmethod: `{}.fromkeys('abc')` must not prepend the
            // receiver dict (it would read as the iterable — the receiver
            // is exact `dict`, so plain-dict construction is right anyway).
            "fromkeys" => Some({
                let mut f = method("fromkeys", dict_fromkeys);
                f.binds_instance = false;
                f
            }),
            "popitem" => Some(method("popitem", dict_popitem)),
            // Dunders so `dict.__setitem__` / `super().__setitem__` resolve
            // for `dict` subclasses (`class C(dict)`).
            "__setitem__" => Some(method("__setitem__", dict_setitem)),
            "__getitem__" => Some(method("__getitem__", dict_getitem)),
            "__delitem__" => Some(method("__delitem__", dict_delitem)),
            // Mapping-protocol dunders exposed as bound methods so code can
            // grab them directly — CPython's `functools._lru_cache_wrapper`
            // caches `cache_len = cache.__len__`, and `__contains__` /
            // `__iter__` round out `hasattr(d, …)` / explicit-call parity.
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__iter__" => Some(method("__iter__", dict_iter_method)),
            "__init__" => Some(method("__init__", dict_update)),
            // PEP 584 merge operators, reachable as explicit methods
            // (`a.__or__(b)`, `a.__ior__(b)`) as well as via `|`/`|=`.
            "__or__" => Some(method("__or__", dict_or)),
            "__ror__" => Some(method("__ror__", dict_ror)),
            "__ior__" => Some(method("__ior__", dict_ior)),
            _ => None,
        },
        Object::Tuple(_) => match name {
            "count" => Some(method("count", tuple_count)),
            "index" => Some(method("index", tuple_index)),
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__add__" => Some(seq_dunder_binop(
                "__add__",
                weavepy_compiler::BinOpKind::Add,
                false,
            )),
            "__mul__" => Some(seq_dunder_binop(
                "__mul__",
                weavepy_compiler::BinOpKind::Mult,
                false,
            )),
            "__rmul__" => Some(seq_dunder_binop(
                "__rmul__",
                weavepy_compiler::BinOpKind::Mult,
                true,
            )),
            _ => None,
        },
        Object::Set(_) | Object::FrozenSet(_) => {
            // The in-place mutators live on `set` only — `frozenset` is
            // immutable, so `hasattr(frozenset(), 'add')` is False (CPython
            // exposes no `set_add`/`set_clear`/… on `PyFrozenSet_Type`).
            // test_set's `test_badcmp` gates its `add`/`discard`/`remove`
            // probes on exactly this `hasattr`.
            let mutators_ok = matches!(obj, Object::Set(_));
            match name {
                "add" if mutators_ok => Some(method("add", set_add)),
                "discard" if mutators_ok => Some(method("discard", set_discard)),
                "remove" if mutators_ok => Some(method("remove", set_remove)),
                "pop" if mutators_ok => Some(method("pop", set_pop)),
                "clear" if mutators_ok => Some(method("clear", set_clear)),
                "update" if mutators_ok => Some(method("update", set_update)),
                "intersection_update" if mutators_ok => {
                    Some(method("intersection_update", set_intersection_update))
                }
                "difference_update" if mutators_ok => {
                    Some(method("difference_update", set_difference_update))
                }
                "symmetric_difference_update" if mutators_ok => Some(method(
                    "symmetric_difference_update",
                    set_symmetric_difference_update,
                )),
                // Methods shared by `set` and `frozenset`.
                "copy" => Some(method("copy", set_copy)),
                "union" => Some(method("union", set_union)),
                "intersection" => Some(method("intersection", set_intersection)),
                "difference" => Some(method("difference", set_difference)),
                "symmetric_difference" => {
                    Some(method("symmetric_difference", set_symmetric_difference))
                }
                "issubset" => Some(method("issubset", set_issubset)),
                "issuperset" => Some(method("issuperset", set_issuperset)),
                "isdisjoint" => Some(method("isdisjoint", set_isdisjoint)),
                // Membership dunder exposed as a bound method: CPython's
                // `keyword.iskeyword = frozenset(kwlist).__contains__` grabs it
                // directly, and `hasattr(s, '__contains__')` must hold.
                "__contains__" => Some(method("__contains__", obj_contains)),
                "__len__" => Some(method("__len__", obj_len)),
                _ => None,
            }
        }
        Object::Bytes(_) | Object::ByteArray(_) => match name {
            "decode" => Some(method_kw("decode", bytes_decode_kw)),
            "hex" => Some(method_kw("hex", bytes_hex_kw)),
            "fromhex" => Some(method("fromhex", bytes_fromhex)),
            "startswith" => Some(method("startswith", bytes_startswith)),
            "endswith" => Some(method("endswith", bytes_endswith)),
            "find" => Some(method("find", bytes_find)),
            "rfind" => Some(method("rfind", bytes_rfind)),
            "index" => Some(method("index", bytes_index)),
            "rindex" => Some(method("rindex", bytes_rindex)),
            "count" => Some(method("count", bytes_count)),
            "lower" => Some(method("lower", bytes_lower)),
            "upper" => Some(method("upper", bytes_upper)),
            "strip" => Some(method("strip", bytes_strip)),
            "lstrip" => Some(method("lstrip", bytes_lstrip)),
            "rstrip" => Some(method("rstrip", bytes_rstrip)),
            "split" => Some(method_kw("split", bytes_split_kw)),
            "rsplit" => Some(method_kw("rsplit", bytes_rsplit_kw)),
            "splitlines" => Some(method_kw("splitlines", bytes_splitlines_kw)),
            "join" => Some(method("join", bytes_join)),
            "replace" => Some(method_kw("replace", bytes_replace_kw)),
            "translate" => Some(method_kw("translate", bytes_translate_kw)),
            "maketrans" => Some(static_method("maketrans", bytes_maketrans)),
            "partition" => Some(method("partition", bytes_partition)),
            "rpartition" => Some(method("rpartition", bytes_rpartition)),
            "removeprefix" => Some(method("removeprefix", bytes_removeprefix)),
            "removesuffix" => Some(method("removesuffix", bytes_removesuffix)),
            "expandtabs" => Some(method_kw("expandtabs", bytes_expandtabs)),
            "center" => Some(method("center", bytes_center)),
            "ljust" => Some(method("ljust", bytes_ljust)),
            "rjust" => Some(method("rjust", bytes_rjust)),
            "zfill" => Some(method("zfill", bytes_zfill)),
            "capitalize" => Some(method("capitalize", bytes_capitalize)),
            "title" => Some(method("title", bytes_title)),
            "swapcase" => Some(method("swapcase", bytes_swapcase)),
            "isalnum" => Some(method("isalnum", bytes_isalnum)),
            "isalpha" => Some(method("isalpha", bytes_isalpha)),
            "isdigit" => Some(method("isdigit", bytes_isdigit)),
            "isspace" => Some(method("isspace", bytes_isspace)),
            "islower" => Some(method("islower", bytes_islower)),
            "isupper" => Some(method("isupper", bytes_isupper)),
            "istitle" => Some(method("istitle", bytes_istitle)),
            "isascii" => Some(method("isascii", bytes_isascii)),
            // bytearray-only mutators
            "append" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("append", bytearray_append))
            }
            "extend" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("extend", bytearray_extend))
            }
            "clear" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("clear", bytearray_clear))
            }
            "pop" if matches!(obj, Object::ByteArray(_)) => Some(method("pop", bytearray_pop)),
            "reverse" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("reverse", bytearray_reverse))
            }
            "insert" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("insert", bytearray_insert))
            }
            "remove" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("remove", bytearray_remove))
            }
            "copy" if matches!(obj, Object::ByteArray(_)) => Some(method("copy", bytearray_copy)),
            // CPython exposes the allocation size (`ob_alloc`, which
            // includes the trailing NUL). We don't track capacity
            // separately, so report `len + 1` — satisfies the documented
            // `__alloc__() > len()` invariant.
            "__alloc__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("__alloc__", |args| {
                    let b = bytearray_self(args)?;
                    let n = b.borrow().len();
                    Ok(Object::Int(n as i64 + 1))
                }))
            }
            // In-place (re)initialisation — `b.__init__(it)` resets the
            // content; subclass `__init__` chains
            // (`bytearray.__init__(me, *args, **kwargs)`) land here too.
            "__init__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method_kw("__init__", bytearray_init_kw))
            }
            // Sequence dunders so direct calls / `hasattr` parity hold.
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__len__" => Some(method("__len__", obj_len)),
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            // Mutable mapping/sequence slots (bytearray only) — delegate to
            // the VM's subscript machinery so int, slice, and `__index__`
            // keys all behave exactly like `b[k] = v` / `del b[k]`
            // (RFC 0056 WS4: CPython stores these in `tp_dict`).
            "__setitem__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("__setitem__", reentrant_setitem))
            }
            "__delitem__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("__delitem__", reentrant_delitem))
            }
            "__iadd__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("__iadd__", bytearray_iadd))
            }
            "__imul__" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("__imul__", bytearray_imul))
            }
            // PEP 688: called when a buffer view over the bytearray is
            // released — validates the view is a live export of this
            // object, then drops it (CPython `wrap_releasebuffer`).
            "__release_buffer__" if matches!(obj, Object::ByteArray(_)) => Some(method(
                "__release_buffer__",
                crate::type_surface::release_buffer_builtin,
            )),
            "__add__" => Some(seq_dunder_binop(
                "__add__",
                weavepy_compiler::BinOpKind::Add,
                false,
            )),
            "__mul__" => Some(seq_dunder_binop(
                "__mul__",
                weavepy_compiler::BinOpKind::Mult,
                false,
            )),
            "__rmul__" => Some(seq_dunder_binop(
                "__rmul__",
                weavepy_compiler::BinOpKind::Mult,
                true,
            )),
            // PEP 461 `%`-formatting exposed as the number-protocol
            // dunders (`bytes_mod` fills CPython's `nb_remainder` slot,
            // so both wrappers exist).
            "__mod__" => Some(method("__mod__", bytes_dunder_mod)),
            "__rmod__" => Some(method("__rmod__", bytes_dunder_rmod)),
            "__bytes__" if matches!(obj, Object::Bytes(_)) => {
                Some(method("__bytes__", |args| match args.first() {
                    Some(Object::Bytes(b)) => Ok(Object::Bytes(b.clone())),
                    _ => Err(type_error("__bytes__ requires a bytes receiver")),
                }))
            }
            _ => None,
        },
        Object::File(f) => match name {
            "read" => Some(method("read", file_read)),
            // `read1` is a *buffered* method (`BufferedReader`/`BufferedWriter`/
            // `BufferedRandom` all expose it via `BufferedIOBase`; a raw `FileIO`
            // genuinely lacks it). Route File-backed buffered streams to
            // `file_read` so a closed stream raises `ValueError` rather than the
            // class-dict `bw_read1` rejecting the `Object::File` receiver
            // (`test_io.test_io_after_close`).
            "read1"
                if matches!(
                    f.io_kind.get(),
                    crate::object::IoKind::BufferedReader
                        | crate::object::IoKind::BufferedWriter
                        | crate::object::IoKind::BufferedRandom
                ) =>
            {
                Some(method("read1", file_read))
            }
            // Binary streams only — CPython text files genuinely lack the
            // attribute (it lives on RawIOBase/BufferedIOBase).
            "readinto" if f.binary => Some(method("readinto", file_readinto)),
            "readinto1" if f.binary => Some(method("readinto1", file_readinto)),
            // `peek` is a buffered-reader method (CPython exposes it on
            // `BufferedReader`/`BufferedRandom` only — a raw `FileIO`, a
            // write-only `BufferedWriter`, or a text stream genuinely lacks it).
            "peek"
                if matches!(
                    f.io_kind.get(),
                    crate::object::IoKind::BufferedReader | crate::object::IoKind::BufferedRandom
                ) =>
            {
                Some(method("peek", file_peek))
            }
            "truncate" => Some(method("truncate", file_truncate)),
            "readline" => Some(method("readline", file_readline)),
            "readlines" => Some(method("readlines", file_readlines)),
            "write" => Some(method("write", file_write)),
            // Routed through the interpreter (sentinel name) so it can
            // consume *any* iterable via the full `__iter__`/`__next__`
            // protocol, not just native sequences.
            "writelines" => Some(method(".file_writelines", file_writelines)),
            "flush" => Some(method("flush", file_flush)),
            // `reconfigure` lives on `TextIOWrapper` only: text-mode
            // OS-backed streams (including stdio). `StringIO` and binary
            // streams genuinely lack it in CPython.
            "reconfigure" if !f.binary && !f.is_memory() => {
                Some(method_kw("reconfigure", file_reconfigure))
            }
            "close" => Some(method("close", file_close)),
            "isatty" => Some(method("isatty", file_isatty)),
            "fileno" => Some(method("fileno", file_fileno)),
            "readable" => Some(method("readable", file_readable)),
            "writable" => Some(method("writable", file_writable)),
            "seekable" => Some(method("seekable", file_seekable)),
            // `IOBase` private protocol helpers the layered `_pyio` Buffered*/
            // TextIOWrapper classes call on the raw stream they wrap.
            "_checkReadable" => Some(method("_checkReadable", file_check_readable)),
            "_checkWritable" => Some(method("_checkWritable", file_check_writable)),
            "_checkSeekable" => Some(method("_checkSeekable", file_check_seekable)),
            "_checkClosed" => Some(method("_checkClosed", file_check_closed)),
            // `RawIOBase.readall` (binary only) — `_pyio.BufferedReader` uses it
            // for a full read of the wrapped native raw.
            "readall" if f.binary => Some(method("readall", file_readall)),
            // In-memory `BytesIO`/`StringIO` streams are picklable and copyable;
            // `file_reduce_mem`/`file_getstate_mem` fall back to the forbidding
            // reducer for file-backed streams, which CPython refuses to pickle
            // (`TypeError: cannot pickle '_io.X' object`). `copy.copy`/
            // `copy.deepcopy` deliberately route through `__reduce_ex__` too
            // (no `__copy__`/`__deepcopy__` shortcut): the reduce path mints a
            // fresh, independent buffer via `cls()` + `__setstate__` and — for a
            // subclass — preserves the user's type and `__dict__` (the
            // native-method binding loses the instance wrapper, so a `__copy__`
            // shortcut would silently downcast to a bare `BytesIO`).
            "__reduce__" => Some(method("__reduce__", file_reduce_mem)),
            "__reduce_ex__" => Some(method("__reduce_ex__", file_reduce_mem)),
            "__getstate__" => Some(method("__getstate__", file_getstate_mem)),
            "__setstate__" if f.is_memory() => Some(method("__setstate__", file_setstate_mem)),
            "seek" => Some(method("seek", file_seek)),
            "tell" => Some(method("tell", file_tell)),
            "getvalue" => Some(method("getvalue", file_getvalue)),
            // `detach()` always refuses on the collapsed native stream:
            // CPython's `BytesIO`/`StringIO`/`TextIOWrapper` raise
            // `UnsupportedOperation` when there is no underlying buffer to
            // hand over (test_memoryio.test_detach).
            "detach" => Some(method("detach", |args| {
                let f = file_self(args)?;
                if *f.closed.borrow() {
                    return Err(value_error("I/O operation on closed file."));
                }
                Err(crate::stdlib::io::unsupported_op("detach"))
            })),
            // `BytesIO.getbuffer()` — binary in-memory streams only (CPython
            // text `StringIO` and file-backed streams genuinely lack the
            // attribute; `hashlib.file_digest` probes it with `hasattr` to
            // pick its zero-copy fast path).
            "getbuffer" if f.binary && f.is_memory() => Some(method("getbuffer", file_getbuffer)),
            "__enter__" => Some(method("__enter__", file_enter)),
            "__exit__" => Some(method("__exit__", file_exit)),
            // A file is its own iterator (CPython): `iter(f) is f`, and
            // each `next(f)` returns the next line, raising StopIteration
            // at EOF.
            "__iter__" => Some(method("__iter__", |args| {
                let f = file_self(args)?;
                // Iterating a closed stream raises (`test_io.test_io_after_close`).
                if *f.closed.borrow() {
                    return Err(value_error("I/O operation on closed file."));
                }
                Ok(Object::File(f))
            })),
            "__next__" => Some(method("__next__", file_next)),
            _ => None,
        },
        Object::MemoryView(_) => match name {
            "tobytes" => Some(method_kw("tobytes", memoryview_tobytes)),
            "tolist" => Some(method("tolist", memoryview_tolist)),
            "toreadonly" => Some(method("toreadonly", memoryview_toreadonly)),
            "release" => Some(method("release", memoryview_release)),
            "cast" => Some(method_kw("cast", memoryview_cast)),
            "hex" => Some(method_kw("hex", memoryview_hex)),
            "__enter__" => Some(method("__enter__", memoryview_enter)),
            "__exit__" => Some(method("__exit__", memoryview_exit)),
            // Sequence/mapping slots as real methods (RFC 0056 WS4):
            // subscripts delegate to the VM machinery so element unpacking,
            // slices, and multi-dim views behave exactly like `mv[k]`.
            "__len__" => Some(method("__len__", obj_len)),
            "__getitem__" => Some(method("__getitem__", reentrant_getitem)),
            "__setitem__" => Some(method("__setitem__", reentrant_setitem)),
            "__delitem__" => Some(method("__delitem__", reentrant_delitem)),
            "__iter__" => Some(method("__iter__", memoryview_iter)),
            // CPython forbids pickling memoryviews at every protocol
            // (test_memoryview.test_pickle).
            "__reduce__" | "__reduce_ex__" => Some(method("__reduce_ex__", |_args| {
                Err(type_error("cannot pickle 'memoryview' object"))
            })),
            "__release_buffer__" => Some(method(
                "__release_buffer__",
                crate::type_surface::release_buffer_builtin,
            )),
            // WeavePy-private: retype a view with an exporter's element
            // format without `cast`'s native-single-char restriction. The
            // pure-Python `array` module uses it so `memoryview(array('u',…))`
            // carries format 'u'/itemsize 4 exactly like CPython's C export
            // (struct can't unpack 'u', which is what makes the comparison
            // semantics of test_buffer's deprecated-u-code test work).
            "_weavepy_with_format" => Some(method("_weavepy_with_format", |args| {
                let mv = memoryview_self(args)?;
                let (Some(Object::Str(fmt)), Some(itemsize)) = (
                    args.get(1),
                    args.get(2).and_then(crate::builtins::try_coerce_index_i64),
                ) else {
                    return Err(type_error(
                        "_weavepy_with_format(format, itemsize) expected (str, int)",
                    ));
                };
                let itemsize = itemsize?.max(1) as usize;
                let nbytes = mv.len.get();
                if nbytes % itemsize != 0 {
                    return Err(value_error(
                        "memoryview: length is not a multiple of itemsize",
                    ));
                }
                let out = mv.shallow_clone();
                *out.format.borrow_mut() = fmt.to_string();
                out.itemsize.set(itemsize);
                let dims = vec![nbytes / itemsize];
                *out.strides.borrow_mut() = crate::object::c_contiguous_strides(&dims, itemsize);
                *out.shape.borrow_mut() = dims;
                out.zero_dim.set(false);
                Ok(Object::MemoryView(Rc::new(out)))
            })),
            // Bound `mv.__eq__(x)` runs CPython `memory_richcompare`: a
            // non-exporter (or one whose getbuffer refuses the FULL_RO
            // request) yields NotImplemented, not False
            // (test_buffer.test_ndarray_getbuf asserts on the sentinel).
            "__eq__" => Some(method("__eq__", |args| {
                let mv = memoryview_self(args)?;
                let other = args
                    .get(1)
                    .ok_or_else(|| type_error("__eq__ expected 1 argument, got 0"))?;
                Ok(match memoryview_eq_option(&mv, other) {
                    Some(eq) => Object::Bool(eq),
                    None => crate::vm_singletons::not_implemented(),
                })
            })),
            "__ne__" => Some(method("__ne__", |args| {
                let mv = memoryview_self(args)?;
                let other = args
                    .get(1)
                    .ok_or_else(|| type_error("__ne__ expected 1 argument, got 0"))?;
                Ok(match memoryview_eq_option(&mv, other) {
                    Some(eq) => Object::Bool(!eq),
                    None => crate::vm_singletons::not_implemented(),
                })
            })),
            // Bound `mv.__hash__()` must run the full `memory_hash`
            // protocol (exporter pre-hash, re-entrancy guard, cache) — the
            // same path as `hash(mv)` (test_memoryview.test_hash_use_after_free
            // calls the bound form directly).
            "__hash__" => Some(method("__hash__", |args| {
                let recv = args
                    .first()
                    .ok_or_else(|| type_error("__hash__() missing self"))?
                    .clone();
                let interp = reentrant_interp()?;
                let globals = interp.builtins_dict();
                interp.do_hash_call(&recv, &globals)
            })),
            _ => None,
        },
        Object::DictView(_) => match name {
            "isdisjoint" => Some(method("isdisjoint", view_isdisjoint)),
            "mapping" => None,
            _ => None,
        },
        // `mappingproxy` (read-only `type.__dict__` view) forwards the
        // read-side mapping API to the wrapped dict.
        Object::MappingProxy(_) | Object::MappingProxyObj(_) => match name {
            "isdisjoint" => Some(method("isdisjoint", view_isdisjoint)),
            "get" => Some(method("get", mappingproxy_get)),
            "keys" => Some(method("keys", mappingproxy_keys)),
            "values" => Some(method("values", mappingproxy_values)),
            "items" => Some(method("items", mappingproxy_items)),
            "copy" => Some(method("copy", mappingproxy_copy)),
            "__getitem__" => Some(method("__getitem__", mappingproxy_getitem)),
            "__len__" => Some(method("__len__", mappingproxy_len)),
            "__contains__" => Some(method("__contains__", mappingproxy_contains)),
            "__iter__" => Some(method("__iter__", mappingproxy_iter)),
            "__reversed__" => Some(method("__reversed__", mappingproxy_reversed)),
            "__or__" => Some(method("__or__", mappingproxy_or)),
            "__ror__" => Some(method("__ror__", mappingproxy_ror)),
            "__ior__" => Some(method("__ior__", mappingproxy_ior)),
            _ => None,
        },
        // NB: GenericAlias / UnionType instances are namespace-shaped
        // too; they must NOT grow the namespace pickle surface (their
        // pickling proxies through `__origin__` instead).
        Object::SimpleNamespace(_) if !crate::is_generic_alias(obj) => match name {
            "__reduce__" | "__reduce_ex__" => Some(method("__reduce__", namespace_reduce)),
            "__replace__" => Some(BuiltinFn {
                name: "__replace__",
                binds_instance: true,
                call: Box::new(|args| namespace_replace(args, &[])),
                call_kw: Some(Box::new(namespace_replace)),
            }),
            _ => None,
        },
        // `property` objects expose `getter`/`setter`/`deleter`
        // methods that return a *new* property carrying a patched
        // function (the underlying decorator pattern), plus the
        // explicit descriptor-protocol slots — CPython's `property` is
        // a data descriptor precisely because its *type* defines
        // `__set__`/`__delete__`, and `inspect.isdatadescriptor`
        // checks exactly that.
        Object::Property(_) => match name {
            "getter" => Some(method("getter", property_getter)),
            "setter" => Some(method("setter", property_setter)),
            "deleter" => Some(method("deleter", property_deleter)),
            // CPython `property_init` re-initialises the descriptor *in
            // place*; a `property` subclass chaining
            // `super().__init__(fget, doc=doc)` (werkzeug's
            // `cached_property`) lands here via super's native-payload
            // probe.
            "__init__" => Some(method_kw("__init__", property_init_kw)),
            "__get__" => Some(method("__get__", property_dunder_get)),
            "__set__" => Some(method("__set__", property_dunder_set)),
            "__delete__" => Some(method("__delete__", property_dunder_delete)),
            // 3.13 (gh-98963): `property.__set_name__(owner, name)` records
            // the attribute name, surfaced as `prop.__name__` and in the
            // "property 'x' of 'C' object has no getter" error family.
            "__set_name__" => Some(method("__set_name__", |args| {
                if args.len() != 3 {
                    return Err(type_error(format!(
                        "__set_name__() takes 2 positional arguments but {} were given",
                        args.len().saturating_sub(1)
                    )));
                }
                if let Some(p) = property_payload(&args[0]) {
                    *p.name.borrow_mut() = Some(args[2].clone());
                }
                Ok(Object::None)
            })),
            "fget" | "fset" | "fdel" | "__doc__" => {
                // These are looked up via `lookup_attr` in the VM
                // rather than method dispatch; we don't return them
                // here.
                None
            }
            _ => None,
        },
        // Non-data descriptor protocol slots, reachable both bound
        // (`sm.__get__`) via `load_attr` and unbound
        // (`staticmethod.__get__`) via the slot-wrapper table.
        Object::StaticMethod(_) => match name {
            "__get__" => Some(method("__get__", staticmethod_descr_get)),
            // 3.10 (bpo-43682): staticmethods are directly callable —
            // `sm(*args)` invokes the wrapped callable.
            "__call__" => Some(method_kw("__call__", staticmethod_call)),
            _ => None,
        },
        Object::ClassMethod(_) => match name {
            "__get__" => Some(method("__get__", classmethod_descr_get)),
            _ => None,
        },
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => match name {
            "bit_length" => Some(method("bit_length", int_bit_length)),
            "bit_count" => Some(method("bit_count", int_bit_count)),
            "to_bytes" => Some(method_kw("to_bytes", int_to_bytes)),
            "from_bytes" => Some(method_kw("from_bytes", int_from_bytes_method)),
            "is_integer" => Some(method("is_integer", int_is_integer)),
            "as_integer_ratio" => Some(method("as_integer_ratio", int_as_integer_ratio)),
            "conjugate" => Some(method("conjugate", int_conjugate)),
            "denominator" | "numerator" | "real" | "imag" => {
                // Property-style access: the VM routes attribute reads
                // through this path too. Return a thunk that yields
                // the value itself.
                None
            }
            "__index__" | "__int__" => Some(method("__index__", int_conjugate)),
            "__trunc__" => Some(method("__trunc__", int_conjugate)),
            "__floor__" => Some(method("__floor__", int_conjugate)),
            "__ceil__" => Some(method("__ceil__", int_conjugate)),
            // `int.__round__([ndigits])` — same shape as the `round()`
            // builtin's native path (typing's SupportsRound probes for it).
            "__round__" => Some(method("__round__", b_round)),
            // `int.__float__` — a real `tp_dict` entry in CPython
            // (RFC 0056 WS4).
            "__float__" => Some(method("__float__", b_float_compat)),
            _ => numeric_dunder(obj, name),
        },
        Object::Float(_) => match name {
            "is_integer" => Some(method("is_integer", float_is_integer)),
            "hex" => Some(method("hex", float_hex)),
            "fromhex" => Some(method("fromhex", float_fromhex)),
            "as_integer_ratio" => Some(method("as_integer_ratio", float_as_integer_ratio)),
            "conjugate" => Some(method("conjugate", float_conjugate)),
            // CPython's `float` exposes `__int__` (truncation toward zero,
            // raising on non-finite values). A C extension reaching `float`'s
            // `nb_int` slot goes through the C-ABI bridge, but the Python-level
            // dunder must exist too (`hasattr(float, "__int__")`).
            "__int__" => Some(method("__int__", float_int)),
            "__trunc__" => Some(method("__trunc__", float_trunc)),
            "__floor__" => Some(method("__floor__", float_floor)),
            "__ceil__" => Some(method("__ceil__", float_ceil)),
            "__round__" => Some(method("__round__", float_round)),
            _ => numeric_dunder(obj, name),
        },
        Object::Complex(_) => match name {
            "conjugate" => Some(method("conjugate", complex_conjugate)),
            // `complex.__complex__(self)` returns the value unchanged, so
            // `complex(x)` / the numeric-tower probes accept a complex.
            "__complex__" => Some(method("__complex__", |args| {
                args.first()
                    .cloned()
                    .ok_or_else(|| crate::error::type_error("__complex__() missing self"))
            })),
            "__abs__" => Some(method("__abs__", |args| {
                b_abs(std::slice::from_ref(args.first().unwrap_or(&Object::None)))
            })),
            _ => numeric_dunder(obj, name),
        },
        Object::Slice(_) => match name {
            "indices" => Some(method("indices", slice_indices_method)),
            // Hashable since 3.12 (gh-101335); the test suite calls the
            // bound `slice(...).__hash__()` form directly.
            "__hash__" => Some(method("__hash__", |args| {
                hash_object(
                    args.first()
                        .ok_or_else(|| type_error("__hash__() missing self"))?,
                )
            })),
            _ => None,
        },
        // Built-in iterators expose `__length_hint__` (PEP 424) so
        // `operator.length_hint`, `list()` pre-sizing, and friends can
        // query the remaining count without consuming the iterator.
        Object::Iter(_) => match name {
            "__length_hint__" => Some(method("__length_hint__", iter_length_hint)),
            "__iter__" => Some(method("__iter__", |args| {
                args.first()
                    .cloned()
                    .ok_or_else(|| type_error("__iter__() missing self"))
            })),
            // `enumerate.__next__` / `reversed.__next__` are real `tp_dict`
            // entries in CPython (RFC 0056 WS4); the load_attr fast path
            // at `Object::Iter` still serves bound instance access.
            "__next__" => Some(method("__next__", |args| match args.first() {
                Some(Object::Iter(it)) => match it.borrow_mut().next_value_checked() {
                    Ok(Some(v)) => Ok(v),
                    Ok(None) => Err(crate::error::stop_iteration()),
                    Err(e) => Err(e),
                },
                // An enumerate/reversed *subclass* over a VM-driven source
                // carries a frozen `_seqtools` iterator instance as its
                // native payload (`MyEnum(getitem_seq)`) — drive it through
                // the interpreter's iteration protocol.
                Some(recv @ Object::Instance(_)) => {
                    let interp = reentrant_interp()?;
                    let globals = interp.builtins_dict();
                    match interp.iter_next(recv, &globals)? {
                        Some(v) => Ok(v),
                        None => Err(crate::error::stop_iteration()),
                    }
                }
                _ => Err(type_error("__next__() requires an iterator")),
            })),
            // Pickling support. The actual reduction needs the canonical
            // `iter` builtin (so the result pickles by name and round-trips),
            // which requires interpreter access — the VM intercepts this
            // sentinel name in its bound-method dispatch.
            "__reduce__" => Some(method(".iter_reduce", |_| {
                Err(type_error("iterator.__reduce__ requires the interpreter"))
            })),
            "__setstate__" => Some(method("__setstate__", iter_setstate)),
            _ => None,
        },
        _ => None,
    };
    f.map(|f| Object::Builtin(Rc::new(f)))
}

/// `<iterator>.__setstate__(index)` — reposition the cursor after an
/// unpickle (CPython's `*iter_setstate`). The index clamps to
/// `[0, len]`; out-of-range states unpickle as an exhausted iterator.
fn iter_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let it = match args.first() {
        Some(Object::Iter(it)) => it.clone(),
        _ => return Err(type_error("__setstate__() requires an iterator")),
    };
    // Any int is a valid state — CPython's `longrangeiter_setstate` accepts
    // (and clamps) values past the machine width, and CPython-produced
    // pickles carry them (test_range.test_iterator_unpickle_compat uses
    // 2**64 + 7). Saturate to i128; the per-variant clamp bounds it anyway.
    let state: i128 = match args.get(1) {
        Some(o) if o.as_i64().is_some() => i128::from(o.as_i64().unwrap()),
        Some(Object::Long(b)) => {
            use num_traits::{Signed, ToPrimitive};
            b.to_i128().unwrap_or(if b.is_negative() {
                i128::MIN
            } else {
                i128::MAX
            })
        }
        _ => return Err(type_error("an integer is required")),
    };
    let clamp = |len: usize| -> usize { state.clamp(0, len as i128) as usize };
    use crate::object::PyIterator;
    match &mut *it.borrow_mut() {
        PyIterator::List { items, index, .. } => {
            *index = clamp(items.borrow().len());
        }
        PyIterator::Tuple { items, index } => *index = clamp(items.len()),
        PyIterator::Str { s, index } => *index = clamp(s.len()),
        PyIterator::Bytes { data, index } => *index = clamp(data.len()),
        PyIterator::ByteArray { data, index } => {
            *index = clamp(data.borrow().len());
        }
        PyIterator::DictKeys { dict, index, .. } => {
            *index = clamp(dict.as_ref().map_or(0, |d| d.borrow().len()));
        }
        PyIterator::Reversed { index, .. } => {
            *index = state.clamp(-1, i128::from(i64::MAX)) as i64;
        }
        // Range iterators keep a moving `current` instead of an index, so
        // repositioning advances `current` by `state` elements (clamped to
        // the remaining length, like CPython's `rangeiter_setstate`). A
        // just-unpickled iterator sits at `start`, so the offset equals
        // CPython's absolute index there.
        PyIterator::Range {
            current,
            stop,
            step,
        } => {
            let (c, s, st) = (i128::from(*current), i128::from(*stop), i128::from(*step));
            let len = range_iter_remaining(c, s, st);
            let n = state.clamp(0, len);
            // `c + n*st` lies in `[current, stop)`, so it round-trips i64.
            *current = if n == len { *stop } else { (c + n * st) as i64 };
        }
        PyIterator::RangeHuge {
            current,
            stop,
            step,
        } => {
            let len = range_iter_remaining(*current, *stop, *step);
            let n = state.clamp(0, len);
            *current = if n == len {
                *stop
            } else {
                *current + n * *step
            };
        }
        _ => {}
    }
    Ok(Object::None)
}

/// Elements a range iterator at `current` will still yield (`0` when
/// exhausted; `step` is never zero for a live range iterator).
fn range_iter_remaining(current: i128, stop: i128, step: i128) -> i128 {
    if step > 0 && current < stop {
        (stop - current + step - 1) / step
    } else if step < 0 && current > stop {
        (current - stop - step - 1) / -step
    } else {
        0
    }
}

/// `<iterator>.__length_hint__()` — the number of items the iterator
/// will still yield, when cheaply known (PEP 424). Returns `0` for
/// exhausted/unknown-length sources, matching CPython's contract that
/// the hint is advisory and never raises.
fn iter_length_hint(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Iter(it)) => {
            let n = it.borrow().remaining().unwrap_or(0);
            Ok(Object::Int(n as i64))
        }
        _ => Err(type_error("__length_hint__() requires an iterator")),
    }
}

/// `seq.__getitem__(self, index)` for built-in sequences — int (incl.
/// negatives) and `slice` indexing for `str`/`list`/`tuple`/`bytes`/
/// `bytearray`. CPython exposes these as slot wrappers; this lets
/// `hasattr(s, '__getitem__')` succeed and direct `str.__getitem__`
/// calls work.
fn seq_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__getitem__() missing self"))?;
    let index = args
        .get(1)
        .ok_or_else(|| type_error("__getitem__() takes exactly one argument (0 given)"))?;
    let as_seq = |v: &Object| -> Vec<Object> {
        match v {
            Object::List(items) => items.borrow().clone(),
            Object::Tuple(items) => items.to_vec(),
            Object::Str(s) => s.chars().map(|c| Object::from_str(c.to_string())).collect(),
            Object::Bytes(b) => b.iter().map(|x| Object::Int(i64::from(*x))).collect(),
            Object::ByteArray(b) => b
                .borrow()
                .iter()
                .map(|x| Object::Int(i64::from(*x)))
                .collect(),
            _ => Vec::new(),
        }
    };
    match index {
        Object::Slice(s) => {
            let seq = as_seq(recv);
            let sliced = crate::slice_seq(&seq, s)?;
            Ok(match recv {
                Object::Str(_) => {
                    Object::from_str(sliced.iter().map(Object::to_str).collect::<String>())
                }
                Object::Tuple(_) => Object::new_tuple(sliced),
                Object::Bytes(_) => {
                    let bytes: Vec<u8> = sliced
                        .iter()
                        .filter_map(|o| o.as_i64())
                        .map(|i| i as u8)
                        .collect();
                    Object::new_bytes(bytes)
                }
                Object::ByteArray(_) => {
                    let bytes: Vec<u8> = sliced
                        .iter()
                        .filter_map(|o| o.as_i64())
                        .map(|i| i as u8)
                        .collect();
                    Object::new_bytearray(bytes)
                }
                _ => Object::new_list(sliced),
            })
        }
        _ => {
            // An index without `__index__` gets the container-specific
            // wording ('abc'.__getitem__('def') — "string indices must be
            // integers, not 'str'", CPython unicode_subscript); a real
            // `__index__` that raises propagates its own error.
            let i = match try_coerce_index_i64(index) {
                Some(res) => res?,
                None => {
                    let t = index.type_name();
                    return Err(type_error(match recv {
                        Object::Str(_) | Object::WStr(_) => {
                            format!("string indices must be integers, not '{t}'")
                        }
                        Object::Bytes(_) => {
                            format!("byte indices must be integers or slices, not {t}")
                        }
                        Object::ByteArray(_) => {
                            format!("bytearray indices must be integers or slices, not {t}")
                        }
                        Object::Tuple(_) => {
                            format!("tuple indices must be integers or slices, not {t}")
                        }
                        Object::List(_) => {
                            format!("list indices must be integers or slices, not {t}")
                        }
                        _ => format!("'{t}' object cannot be interpreted as an integer"),
                    }));
                }
            };
            let seq = as_seq(recv);
            // Match CPython's per-type `IndexError` text (`sq_item` wrappers);
            // `bytes` (and any fallback) stays bare `"index out of range"`.
            let msg = match recv {
                Object::List(_) => "list index out of range",
                Object::Tuple(_) => "tuple index out of range",
                Object::Str(_) => "string index out of range",
                Object::ByteArray(_) => "bytearray index out of range",
                _ => "index out of range",
            };
            let idx = crate::normalize_index_msg(i, seq.len(), msg)?;
            Ok(seq[idx].clone())
        }
    }
}

/// `obj.__len__(self)` for built-in containers.
fn obj_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__len__() missing self"))?;
    Ok(Object::Int(recv.len()? as i64))
}

/// `obj.__contains__(self, item)` for built-in containers.
fn obj_contains(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__contains__() missing self"))?;
    let item = args
        .get(1)
        .ok_or_else(|| type_error("__contains__() takes exactly one argument (0 given)"))?;
    Ok(Object::Bool(recv.contains(item)?))
}

/// `slice.indices(length)` → the `(start, stop, step)` triple a sequence
/// of `length` items would use, mirroring CPython's `PySlice_Unpack` +
/// `PySlice_AdjustIndices` (`Objects/sliceobject.c`). `length` must be a
/// non-negative integer (or `__index__`-able); `step` of 0 is a
/// `ValueError`.
fn slice_indices_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Slice(s)) => s.clone(),
        _ => return Err(type_error("descriptor 'indices' requires a 'slice' object")),
    };
    // CPython's `_PySlice_GetLongIndices`: everything is arbitrary-
    // precision integer arithmetic — start/stop/step/length beyond
    // `sys.maxsize` clamp exactly, never overflow (issue #14794;
    // test_slice.test_indices sweeps `±2**100`).
    use num_bigint::BigInt;
    let to_big = |o: &Object| -> Result<BigInt, RuntimeError> {
        match coerce_index_object(o)? {
            Object::Int(i) => Ok(BigInt::from(i)),
            Object::Long(b) => Ok((*b).clone()),
            _ => unreachable!("coerce_index_object returns Int or Long"),
        }
    };
    let big_obj = |v: BigInt| -> Object {
        match i64::try_from(&v) {
            Ok(i) => Object::Int(i),
            Err(_) => Object::Long(Rc::new(v)),
        }
    };
    let length = match args.get(1) {
        Some(o) => to_big(o)?,
        None => return Err(type_error("indices() takes exactly one argument (0 given)")),
    };
    let zero = BigInt::from(0);
    if length < zero {
        return Err(value_error("length should not be negative"));
    }
    let step = match &s.step {
        Object::None => BigInt::from(1),
        o => {
            let st = to_big(o)?;
            if st == zero {
                return Err(value_error("slice step cannot be zero"));
            }
            st
        }
    };
    let backwards = step < zero;
    let (lower, upper) = if backwards {
        (BigInt::from(-1), &length + BigInt::from(-1))
    } else {
        (zero.clone(), length.clone())
    };
    let clamp = |v: BigInt| -> BigInt {
        if v < zero {
            (v + &length).max(lower.clone())
        } else {
            v.min(upper.clone())
        }
    };
    let start = match &s.start {
        Object::None => {
            if backwards {
                upper.clone()
            } else {
                lower.clone()
            }
        }
        o => clamp(to_big(o)?),
    };
    let stop = match &s.stop {
        Object::None => {
            if backwards {
                lower.clone()
            } else {
                upper.clone()
            }
        }
        o => clamp(to_big(o)?),
    };
    Ok(Object::new_tuple(vec![
        big_obj(start),
        big_obj(stop),
        big_obj(step),
    ]))
}

fn method(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) -> BuiltinFn {
    BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }
}

/// Like [`method`] but for CPython *static methods* reached through an
/// instance (`'abc'.maketrans(d)`): the receiver must not be prepended
/// to the call arguments (shlex builds its punctuation table with
/// `self.wordchars.maketrans(dict.fromkeys(...))`).
fn static_method(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) -> BuiltinFn {
    BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }
}

// ---- sequence slot-wrapper dunders (`str.__add__`, `list.__mul__`, …) ----
//
// CPython exposes the sequence protocol's binary slots as wrapper
// descriptors on each concrete type (`str.__add__`, `list.__mul__`,
// `tuple.__rmul__`, …). Like the C wrappers, they delegate straight to
// the native operation — its TypeErrors ("can only concatenate str …")
// surface unchanged. Reached only via attribute access; the `a + b`
// operator path never routes primitives through here.
fn seq_dunder_binop(
    name: &'static str,
    op: weavepy_compiler::BinOpKind,
    reflected: bool,
) -> BuiltinFn {
    method(name, move |args: &[Object]| {
        let (a, b) = match args {
            [a, b] => (a, b),
            _ => {
                return Err(type_error(format!(
                    "{name} expected 2 arguments, got {}",
                    args.len().saturating_sub(1)
                )))
            }
        };
        let (a, b) = if reflected { (b, a) } else { (a, b) };
        // `str.__mod__` reached as a slot wrapper (markupsafe's
        // `super().__mod__(arg)`) still needs `%s`/`%r` of user
        // instances to dispatch `__str__`/`__repr__` — route through
        // the VM formatter when one is live, like the `%` opcode does.
        if matches!(op, weavepy_compiler::BinOpKind::Mod) && a.is_str() {
            if let Ok(interp) = reentrant_interp() {
                let globals = interp.builtins_dict();
                return interp.percent_mod_left_slot(a, b, &globals);
            }
        }
        crate::binary_op(a, b, op)
    })
}

// ---- numeric slot-wrapper dunders (`int.__add__`, `complex.__eq__`, …) ----
//
// CPython exposes every numeric operator as a method on its type
// (`int.__add__`, `(1+2j).__truediv__`, …) that follows the binary-op
// protocol: when the *other* operand isn't a type the forward operation
// accepts, the wrapper returns `NotImplemented` instead of raising. These
// wrappers reproduce that so explicit dunder calls match CPython.
//
// They are reached only through *attribute access* — `type.__op__` (via
// [`unbound_method`]) and `value.__op__` (via [`lookup_method`]). The hot
// `a + b` operator path dispatches through `instance_method`, which only
// matches user `Object::Instance`, so primitives never route their `+`
// through here and there is neither extra overhead nor recursion risk.

#[derive(Clone, Copy)]
enum NumSelf {
    Int,
    Float,
    Complex,
}

/// Classify a numeric receiver (unwrapping a built-in subclass to its
/// native payload). Non-numerics return `None`.
fn num_self_of(o: &Object) -> Option<NumSelf> {
    let native = o.native_value();
    match native.as_ref().unwrap_or(o) {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => Some(NumSelf::Int),
        Object::Float(_) => Some(NumSelf::Float),
        Object::Complex(_) => Some(NumSelf::Complex),
        _ => None,
    }
}

/// Does the forward dunder of `kind` accept `other`? Mirrors CPython's
/// numeric coercion ladder: `int` accepts only ints, `float` also accepts
/// floats, `complex` also accepts complexes.
fn num_accepts(kind: NumSelf, other: &Object) -> bool {
    let native = other.native_value();
    let o = native.as_ref().unwrap_or(other);
    let is_int = matches!(o, Object::Int(_) | Object::Long(_) | Object::Bool(_));
    let is_float = matches!(o, Object::Float(_));
    let is_complex = matches!(o, Object::Complex(_));
    match kind {
        NumSelf::Int => is_int,
        NumSelf::Float => is_int || is_float,
        NumSelf::Complex => is_int || is_float || is_complex,
    }
}

#[derive(Clone, Copy)]
enum CmpDun {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Build a binary-arithmetic dunder (`__add__`, `__rmul__`, …).
fn num_binop_method(
    nm: &'static str,
    kind: NumSelf,
    op: weavepy_compiler::BinOpKind,
    reflected: bool,
) -> BuiltinFn {
    method(nm, move |args| {
        let s = args
            .first()
            .cloned()
            .ok_or_else(|| type_error(format!("unbound method {nm}() needs an argument")))?;
        let o = match args.get(1) {
            Some(o) => o.clone(),
            None => return Err(type_error(format!("{nm}() takes exactly one argument"))),
        };
        if !num_accepts(kind, &o) {
            return Ok(crate::vm_singletons::not_implemented());
        }
        let (l, r) = if reflected { (&o, &s) } else { (&s, &o) };
        crate::binary_op(l, r, op)
    })
}

/// Build a rich-comparison dunder (`__eq__`, `__lt__`, …).
fn num_cmp_method(nm: &'static str, kind: NumSelf, which: CmpDun) -> BuiltinFn {
    method(nm, move |args| {
        let s = args
            .first()
            .cloned()
            .ok_or_else(|| type_error(format!("unbound method {nm}() needs an argument")))?;
        let o = match args.get(1) {
            Some(o) => o.clone(),
            None => return Err(type_error(format!("{nm}() takes exactly one argument"))),
        };
        let ordering = matches!(which, CmpDun::Lt | CmpDun::Le | CmpDun::Gt | CmpDun::Ge);
        // `complex` has no ordering: `<`/`<=`/`>`/`>=` always decline.
        if ordering && matches!(kind, NumSelf::Complex) {
            return Ok(crate::vm_singletons::not_implemented());
        }
        if !num_accepts(kind, &o) {
            return Ok(crate::vm_singletons::not_implemented());
        }
        let result = match which {
            CmpDun::Eq => s.eq_value(&o),
            CmpDun::Ne => !s.eq_value(&o),
            CmpDun::Lt | CmpDun::Le | CmpDun::Gt | CmpDun::Ge => match s.cmp(&o) {
                Ok(ord) => match which {
                    CmpDun::Lt => ord.is_lt(),
                    CmpDun::Le => ord.is_le(),
                    CmpDun::Gt => ord.is_gt(),
                    CmpDun::Ge => ord.is_ge(),
                    _ => unreachable!(),
                },
                // Unorderable (NaN) → CPython yields `False`, not an error.
                Err(_) => false,
            },
        };
        Ok(Object::Bool(result))
    })
}

/// Build a unary dunder (`__neg__`, `__pos__`, `__abs__`).
fn num_unary_method(nm: &'static str, op: weavepy_compiler::UnaryKind) -> BuiltinFn {
    method(nm, move |args| {
        let s = args
            .first()
            .cloned()
            .ok_or_else(|| type_error(format!("unbound method {nm}() needs an argument")))?;
        // Subclass instances apply the base type's op to their native
        // payload (CPython's inherited slot ignores the subclass).
        let s = s.native_value().unwrap_or(s);
        crate::unary_op(&s, op)
    })
}

/// `(value).__getnewargs__()` for the built-in numerics: `complex`
/// reconstructs from `(real, imag)`, the rest from `(value,)`.
fn num_getnewargs(self_o: &Object) -> Object {
    let native = self_o.native_value();
    let v = native.as_ref().unwrap_or(self_o);
    match v {
        Object::Complex(c) => Object::new_tuple(vec![Object::Float(c.real), Object::Float(c.imag)]),
        other => Object::new_tuple(vec![other.clone()]),
    }
}

/// Resolve a numeric slot-wrapper dunder by name for receiver `self_repr`.
/// Returns `None` for anything that isn't a numeric dunder so the caller
/// falls through to its other attribute paths.
pub(crate) fn numeric_dunder(self_repr: &Object, name: &str) -> Option<BuiltinFn> {
    use weavepy_compiler::BinOpKind as B;
    use weavepy_compiler::UnaryKind as U;
    let kind = num_self_of(self_repr)?;
    let not_complex = !matches!(kind, NumSelf::Complex);
    let m = match name {
        "__add__" => num_binop_method("__add__", kind, B::Add, false),
        "__radd__" => num_binop_method("__radd__", kind, B::Add, true),
        "__sub__" => num_binop_method("__sub__", kind, B::Sub, false),
        "__rsub__" => num_binop_method("__rsub__", kind, B::Sub, true),
        "__mul__" => num_binop_method("__mul__", kind, B::Mult, false),
        "__rmul__" => num_binop_method("__rmul__", kind, B::Mult, true),
        "__truediv__" => num_binop_method("__truediv__", kind, B::Div, false),
        "__rtruediv__" => num_binop_method("__rtruediv__", kind, B::Div, true),
        // `__pow__` takes CPython's optional modulus (`(2).__pow__(3, 7)`
        // is `pow(2, 3, 7)` — the slot wrapper forwards all of
        // `nb_power`'s ternary form; test_inspect test_signature_on_class
        // [MethodWrapperType]).
        "__pow__" => method("__pow__", move |args| {
            let s = args
                .first()
                .cloned()
                .ok_or_else(|| type_error("unbound method __pow__() needs an argument"))?;
            let o = match args.get(1) {
                Some(o) => o.clone(),
                None => return Err(type_error("__pow__() takes exactly one argument")),
            };
            if !num_accepts(kind, &o) {
                return Ok(crate::vm_singletons::not_implemented());
            }
            match args.get(2) {
                Some(m) if !matches!(m, Object::None) => b_pow(&[s, o, m.clone()]),
                _ => crate::binary_op(&s, &o, B::Pow),
            }
        }),
        "__rpow__" => num_binop_method("__rpow__", kind, B::Pow, true),
        // `floordiv`/`mod` are undefined on `complex`.
        "__floordiv__" if not_complex => num_binop_method("__floordiv__", kind, B::FloorDiv, false),
        "__rfloordiv__" if not_complex => {
            num_binop_method("__rfloordiv__", kind, B::FloorDiv, true)
        }
        "__mod__" if not_complex => num_binop_method("__mod__", kind, B::Mod, false),
        "__rmod__" if not_complex => num_binop_method("__rmod__", kind, B::Mod, true),
        // Bitwise / shift ops are int-only (CPython `long_*` slots).
        "__lshift__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__lshift__", kind, B::LShift, false)
        }
        "__rlshift__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__rlshift__", kind, B::LShift, true)
        }
        "__rshift__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__rshift__", kind, B::RShift, false)
        }
        "__rrshift__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__rrshift__", kind, B::RShift, true)
        }
        "__and__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__and__", kind, B::BitAnd, false)
        }
        "__rand__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__rand__", kind, B::BitAnd, true)
        }
        "__or__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__or__", kind, B::BitOr, false)
        }
        "__ror__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__ror__", kind, B::BitOr, true)
        }
        "__xor__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__xor__", kind, B::BitXor, false)
        }
        "__rxor__" if matches!(kind, NumSelf::Int) => {
            num_binop_method("__rxor__", kind, B::BitXor, true)
        }
        "__divmod__" if not_complex => method("__divmod__", move |args| {
            let (a, b) = match args {
                [a, b] => (a.clone(), b.clone()),
                _ => return Err(type_error("__divmod__ expected 2 arguments")),
            };
            if !num_accepts(kind, &b) {
                return Ok(crate::vm_singletons::not_implemented());
            }
            b_divmod(&[a, b])
        }),
        "__rdivmod__" if not_complex => method("__rdivmod__", move |args| {
            let (a, b) = match args {
                [a, b] => (a.clone(), b.clone()),
                _ => return Err(type_error("__rdivmod__ expected 2 arguments")),
            };
            if !num_accepts(kind, &b) {
                return Ok(crate::vm_singletons::not_implemented());
            }
            b_divmod(&[b, a])
        }),
        "__invert__" if matches!(kind, NumSelf::Int) => num_unary_method("__invert__", U::Invert),
        "__bool__" => method("__bool__", |args| {
            let v = args.first().cloned().unwrap_or(Object::None);
            let v = v.native_value().unwrap_or(v);
            Ok(Object::Bool(v.is_truthy()))
        }),
        "__abs__" => method("__abs__", |args| {
            let v = args.first().cloned().unwrap_or(Object::None);
            // Unwrap a builtin-subclass receiver to its native payload
            // (`abs(IntSubclass(0))` routes here via the type-dict slot).
            let v = v.native_value().unwrap_or(v);
            b_abs(std::slice::from_ref(&v))
        }),
        "__eq__" => num_cmp_method("__eq__", kind, CmpDun::Eq),
        "__ne__" => num_cmp_method("__ne__", kind, CmpDun::Ne),
        "__lt__" => num_cmp_method("__lt__", kind, CmpDun::Lt),
        "__le__" => num_cmp_method("__le__", kind, CmpDun::Le),
        "__gt__" => num_cmp_method("__gt__", kind, CmpDun::Gt),
        "__ge__" => num_cmp_method("__ge__", kind, CmpDun::Ge),
        "__neg__" => num_unary_method("__neg__", U::Neg),
        "__pos__" => num_unary_method("__pos__", U::Pos),
        "__getnewargs__" => method("__getnewargs__", |args| {
            Ok(num_getnewargs(args.first().unwrap_or(&Object::None)))
        }),
        "__format__" => method("__format__", |args| {
            let value = args.first().cloned().unwrap_or(Object::None);
            let spec = match args.get(1) {
                Some(Object::Str(s)) => s.to_string(),
                Some(other) => {
                    return Err(type_error(format!(
                        "__format__() argument 1 must be str, not {}",
                        other.type_name()
                    )))
                }
                None => String::new(),
            };
            // CPython: an empty spec is `PyObject_Str(self)` — a *virtual*
            // call, so an `IntEnum` member with an overridden `__str__`
            // formats through that override, not its int payload.
            if spec.is_empty() {
                return virtual_format_str(&value);
            }
            // A non-empty spec formats the native payload — e.g.
            // `int.__format__(member, 'd')` is `'3'`, never the repr.
            let value = value.native_value().unwrap_or(value);
            crate::format_via_spec(&value, &spec).map(Object::from_str)
        }),
        // Exposing the numeric `__hash__` puts it in the type's MRO so a
        // mixin like `class F(float, H)` resolves `float.__hash__` (not
        // `H.__hash__`), matching CPython's method resolution.
        "__hash__" => method("__hash__", |args| {
            hash_object(args.first().unwrap_or(&Object::None))
        }),
        _ => return None,
    };
    Some(m)
}

/// `value.__getnewargs__()` for an immutable built-in subclass instance:
/// returns `(value,)` so `copy`/`pickle` reconstruct it as
/// `cls.__new__(cls, value)`. The receiver (`args[0]`) is the subclass
/// instance; its wrapped native payload is the base-type value.
fn instance_getnewargs(args: &[Object]) -> Result<Object, RuntimeError> {
    let native = match args.first() {
        Some(Object::Instance(inst)) => inst.native.get().cloned(),
        other => other.cloned(),
    };
    match native {
        // `unicode_getnewargs` builds a *fresh* string
        // (test_str.test_getnewargs asserts `args[0] is not text`), so don't
        // hand back the same allocation.
        Some(Object::Str(s)) => Ok(Object::new_tuple(vec![Object::Str(Rc::from(&*s))])),
        Some(Object::WStr(cps)) => Ok(Object::new_tuple(vec![Object::WStr(cps.to_vec().into())])),
        Some(v) => Ok(Object::new_tuple(vec![v])),
        None => Ok(Object::new_tuple(Vec::new())),
    }
}

/// `__getnewargs__` for a subclass of an immutable built-in whose
/// reconstruction takes a single positional value (`int`/`float`/`str`/
/// `bytes`/`tuple`/`bool`). Returns `None` for everything else: mutable
/// containers rebuild from items/state, `frozenset`/`set` have no
/// `__getnewargs__` in CPython, and `complex` uses a two-arg `(re, im)`
/// form handled separately.
pub fn immutable_subclass_getnewargs(native: &Object) -> Option<Object> {
    let single_value = matches!(
        native,
        Object::Int(_)
            | Object::Long(_)
            | Object::Bool(_)
            | Object::Float(_)
            | Object::Str(_)
            | Object::Bytes(_)
            | Object::Tuple(_)
    );
    single_value.then(|| Object::Builtin(Rc::new(method("__getnewargs__", instance_getnewargs))))
}

/// The `__getnewargs__` method-descriptor materialized in the *type dict*
/// of the immutable sequence built-ins (`tuple`/`str`/`bytes`). CPython
/// exposes `tuple.__getnewargs__` / `str.__getnewargs__` /
/// `bytes.__getnewargs__` as real `tp_methods` entries; the numeric types
/// get theirs separately via [`numeric_dunder`]. Materializing them lets
/// the *type-only* lookup CPython's `object.__reduce_ex__` performs
/// (`_PyObject_LookupSpecial`, ported as `copyreg._lookup_special`) find
/// the hook, so `copy`/`pickle` reconstruct `cls.__new__(cls, value)`
/// instead of an empty instance. Bound to the receiver at call time, it
/// returns `(value,)` (the native payload for a subclass instance).
pub fn immutable_getnewargs_method() -> Object {
    Object::Builtin(Rc::new(method("__getnewargs__", instance_getnewargs)))
}

/// Like [`method`] but for builtins that accept keyword arguments. The
/// body receives the positional args (with the bound receiver at index
/// 0) *and* the keyword pairs, so it can implement CPython's mixed
/// positional/keyword signatures (e.g. `str.split(sep=None, maxsplit=-1)`,
/// `str.splitlines(keepends=False)`).
fn method_kw(
    name: &'static str,
    body: impl Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>
        + Send
        + Sync
        + 'static,
) -> BuiltinFn {
    let body = std::sync::Arc::new(body);
    let positional = body.clone();
    BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(move |args| positional(args, &[])),
        call_kw: Some(Box::new(move |args, kwargs| body(args, kwargs))),
    }
}

/// Resolve a parameter that may be passed positionally (`args[pos]`) or
/// by keyword (`kwargs[name]`). Positional wins; returns `None` when the
/// argument is absent so the caller can apply its default.
fn arg_or_kw<'a>(
    args: &'a [Object],
    pos: usize,
    kwargs: &'a [(String, Object)],
    name: &str,
) -> Option<&'a Object> {
    if let Some(v) = args.get(pos) {
        return Some(v);
    }
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Built-in classmethod / staticmethod table: `Type.name` access for
/// names not stored in the type's ``dict`` (e.g. `str.maketrans`,
/// `bytes.fromhex`, `int.from_bytes`, `dict.fromkeys`,
/// `float.fromhex`, `bytes.maketrans`). Returns an unbound builtin
/// so the call site supplies the arguments unchanged.
pub fn builtin_classmethod(type_name: &str, attr: &str) -> Option<Object> {
    let f = match (type_name, attr) {
        ("str", "maketrans") => Some(method("maketrans", str_maketrans)),
        ("bytes", "fromhex") | ("bytearray", "fromhex") => Some(method("fromhex", bytes_fromhex)),
        ("int", "from_bytes") => Some(method_kw("from_bytes", int_from_bytes_method)),
        ("float", "fromhex") => Some(method("fromhex", float_fromhex)),
        ("dict", "fromkeys") => Some(method("fromkeys", dict_fromkeys)),
        _ => None,
    };
    // These are CPython class/static methods: `dict.fromkeys`, `str.maketrans`,
    // etc. are already "bound" (to the type) when read off the type, and they
    // are *not* instance descriptors. Each body scans its arguments from slot
    // 0, so they never need a prepended receiver. Mark them `binds_instance =
    // false` so that when one is stashed as a class attribute and later read
    // through an *instance* (`class C: ctor = dict.fromkeys; C().ctor(it)`),
    // `maybe_bind` returns it unchanged instead of wrongly rebinding it to the
    // instance (which made `self` look like the iterable — bpo-46615's
    // `TestMethodsMutating_Set_Dict`).
    f.map(|mut f| {
        f.binds_instance = false;
        Object::Builtin(Rc::new(f))
    })
}

/// Unbound-method access on a built-in type, e.g. `str.upper`, `float.hex`,
/// `list.append`. CPython exposes every instance method as an attribute of
/// its type that takes the receiver as an explicit first argument; the
/// `BuiltinFn`s in [`lookup_method`] already treat `args[0]` as `self`, so
/// the same function object serves both bound (`x.upper()`) and unbound
/// (`str.upper(x)`) call forms. We synthesise a throw-away representative of
/// the type purely so the variant-based dispatch in [`lookup_method`] can
/// pick the right table — the value is never inspected.
pub fn unbound_method(type_name: &str, name: &str) -> Option<Object> {
    let rep: Object = match type_name {
        "str" => Object::from_static(""),
        "float" => Object::Float(0.0),
        "int" => Object::Int(0),
        "bool" => Object::Bool(false),
        "complex" => Object::new_complex(0.0, 0.0),
        "bytes" => Object::new_bytes(Vec::<u8>::new()),
        "bytearray" => Object::new_bytearray(Vec::<u8>::new()),
        "list" => Object::new_list(Vec::new()),
        "tuple" => Object::new_tuple(Vec::new()),
        "dict" => Object::new_dict(),
        "set" => Object::new_set(),
        "frozenset" => Object::new_frozenset_from(std::iter::empty::<Object>()),
        // A representative (empty) iterator so `type(it).__length_hint__`
        // resolves to the unbound slot wrapper; the actual call receives the
        // real iterator as `self`. `operator.length_hint` reaches it this way.
        "iterator" => Object::Iter(Rc::new(RefCell::new(crate::object::PyIterator::Tuple {
            items: Rc::from(Vec::<Object>::new()),
            index: 0,
        }))),
        // `enumerate` / `reversed` are real types whose instances share
        // the built-in iterator method table (`__length_hint__`,
        // `__reduce__`, …); a representative empty instance routes the
        // type-level lookup there.
        "enumerate" => Object::Iter(Rc::new(RefCell::new(
            crate::object::PyIterator::Enumerate {
                inner: Rc::new(RefCell::new(crate::object::PyIterator::Tuple {
                    items: Rc::from(Vec::<Object>::new()),
                    index: 0,
                })),
                count: 0,
                count_big: None,
            },
        ))),
        "reversed" => Object::Iter(Rc::new(RefCell::new(crate::object::PyIterator::Reversed {
            items: Rc::new(RefCell::new(Vec::new())),
            index: -1,
            owner: None,
        }))),
        // Descriptor types: expose their protocol slots
        // (`property.__set__`, `staticmethod.__get__`, …) for
        // type-level access; the call receives the real descriptor as
        // `self` via `args[0]`.
        "property" => Object::Property(Rc::new(crate::object::PyProperty::new(
            Object::None,
            Object::None,
            Object::None,
            Object::None,
        ))),
        "staticmethod" => Object::StaticMethod(MethodWrapper::new(Object::None)),
        "classmethod" => Object::ClassMethod(MethodWrapper::new(Object::None)),
        // Value/container types whose instances resolve methods through
        // `lookup_method` directly (RFC 0056 WS4): representatives route
        // type-level access (`memoryview.hex`, `range.count`,
        // `slice.indices`) to the same tables.
        "memoryview" => Object::MemoryView(Rc::new(crate::object::PyMemoryView::from_bytes(
            Rc::from(Vec::<u8>::new()),
        ))),
        "range" => Object::Range(Rc::new(crate::object::Range::new(0, 0, 1))),
        "slice" => Object::Slice(Rc::new(crate::object::PySlice {
            start: Object::None,
            stop: Object::None,
            step: Object::None,
        })),
        _ => return None,
    };
    lookup_method(&rep, name)
}

// ---- universal object-protocol slot wrappers (`object.__repr__`, …) ----
//
// CPython stores a slot wrapper for the object protocol in every type's
// `tp_dict` (`object.__repr__`, `int.__str__`, `str.__format__`, …). WeavePy
// synthesizes these on demand for *type-level* attribute access only (the
// instance path keeps using `repr_of` / `stringify`), and the caller caches
// the result per `(type, name)` so identity is stable — `enum`'s bootstrap
// compares `getattr(member_type, '__str__') is object.__str__` and
// `found_method in (data_type_method, object_method)`.

/// `object.__repr__(self)` / `int.__repr__(self)` / … — the default repr of
/// `self`, unwrapping a built-in subclass's native payload first (so
/// `int.__repr__(IntEnumMember)` renders the wrapped integer).
fn slot_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = args
        .first()
        .ok_or_else(|| type_error("__repr__() takes exactly one argument (0 given)"))?;
    // CPython guards `PyObject_Repr` with `Py_EnterRecursiveCall`; the
    // native repr can re-enter the VM (user `__repr__`), and rebinding
    // `__repr__ = __str__` (test_descr.test_repr_as_str) creates a
    // native-only cycle that must raise instead of overflowing.
    let _guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => {
            return Err(crate::error::recursion_error(
                "maximum recursion depth exceeded while getting the repr of an object",
            ))
        }
    };
    let native = o.native_value();
    // CPython's `bytearray_repr` spells the *receiver's* type name
    // (`_PyType_Name(Py_TYPE(self))`), so a bytearray subclass renders
    // as `ByteArraySubclass(b'…')` even without a custom `__repr__`.
    if let (Object::Instance(inst), Some(payload @ Object::ByteArray(_))) = (o, &native) {
        let name = inst.cls().name.clone();
        let short = name.rsplit('.').next().unwrap_or(&name);
        let inner = payload.repr();
        let body = inner
            .strip_prefix("bytearray(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(&inner);
        return Ok(Object::from_str(format!("{short}({body})")));
    }
    // `set`/`frozenset` likewise spell the receiver's own type name
    // (CPython `set_repr`: `Py_TYPE(so) != &PySet_Type`), so an explicit
    // `set.__repr__(subclass_instance)` renders `set3({…})`. Rendering the
    // native payload here (not re-dispatching `o.repr()`) avoids recursing
    // back into a user `__repr__` that delegates to `set.__repr__`.
    if let (Object::Instance(inst), Some(native_set @ (Object::Set(_) | Object::FrozenSet(_)))) =
        (o, &native)
    {
        let name = inst.cls().name.clone();
        let short = name.rsplit('.').next().unwrap_or(&name);
        if let Some(rendered) = crate::object::set_repr_tagged(native_set, short) {
            return Ok(Object::from_str(rendered));
        }
    }
    Ok(Object::from_str(native.as_ref().unwrap_or(o).repr()))
}

/// `str.__str__(self)` / `object.__str__(self)` — `str()` of `self`. Mirrors
/// CPython: for a value that doesn't define its own `__str__`, this is the
/// `repr`-derived default; for `str`/`bytes` it returns the payload.
fn slot_str(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = args
        .first()
        .ok_or_else(|| type_error("__str__() takes exactly one argument (0 given)"))?;
    // See `slot_repr`: participate in the recursion limit so
    // `__repr__`/`__str__` rebinding cycles raise `RecursionError`.
    let _guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => {
            return Err(crate::error::recursion_error(
                "maximum recursion depth exceeded while getting the str of an object",
            ))
        }
    };
    // CPython `object.__str__` is `PyObject_Repr(self)`: a user-defined
    // `__repr__` is dispatched through the VM so its exceptions (and
    // RecursionError from `__repr__ = __str__` cycles) *propagate*,
    // rather than being swallowed by the native fallback rendering.
    // The check runs on the instance itself — *before* unwrapping any
    // native payload — so `__str__ = object.__str__` on an `IntEnum`
    // still routes through the member's `__repr__` rather than the
    // wrapped int's rendering.
    if let Object::Instance(inst) = o {
        let key = crate::object::DictKey(Object::from_static("__repr__"));
        let has_user_repr = inst
            .cls()
            .mro
            .borrow()
            .iter()
            .any(|t| t.dict.borrow().contains_key(&key));
        if has_user_repr {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame still live
                // on this thread; the GIL keeps it exclusive.
                let interp = unsafe { &mut *ptr };
                if let Some(method) = crate::instance_method(o, "__repr__") {
                    let globals = interp.builtins_dict();
                    let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                    return Ok(Object::from_str(r.to_str()));
                }
            }
        }
    }
    let native = o.native_value();
    let target = native.as_ref().unwrap_or(o);
    Ok(Object::from_str(target.to_str()))
}

/// `object.__format__(self, spec)` / `str.__format__(self, spec)` — format
/// `self` per `spec`, unwrapping a native payload first. An empty spec is
/// equivalent to `str(self)`.
fn slot_format(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = args
        .first()
        .ok_or_else(|| type_error("__format__() takes exactly 2 arguments (0 given)"))?;
    let spec = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        // A `str` subclass is a legal spec (PyUnicode_Check).
        Some(Object::Instance(inst)) if matches!(inst.native.get(), Some(Object::Str(_))) => {
            match inst.native.get() {
                Some(Object::Str(s)) => s.to_string(),
                _ => unreachable!(),
            }
        }
        None => String::new(),
        // CPython's `object.__format__` requires a str spec — `None` too
        // is a TypeError (test_builtin test_format).
        Some(other) => {
            return Err(type_error(format!(
                "__format__() argument 1 must be str, not {}",
                other.type_name()
            )))
        }
    };
    // Empty spec ≡ `str(self)` — dispatched virtually so user `__str__`
    // overrides on built-in subclasses are honoured (CPython behaviour).
    if spec.is_empty() {
        return virtual_format_str(o);
    }
    let native = o.native_value();
    crate::format_via_spec(native.as_ref().unwrap_or(o), &spec).map(Object::from_str)
}

/// `format(x, '')` semantics shared by the built-in `__format__` slot
/// wrappers: CPython's `<type>.__format__(self, '')` short-circuits to
/// `PyObject_Str(self)`, a *virtual* str() that dispatches a user
/// `__str__`/`__repr__` override before falling back to the native
/// payload's rendering.
fn virtual_format_str(o: &Object) -> Result<Object, RuntimeError> {
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        return interp.stringify_public(o, &globals).map(Object::from_str);
    }
    let native = o.native_value();
    Ok(Object::from_str(native.as_ref().unwrap_or(o).to_str()))
}

/// `type.__call__` / `function.__call__` / … — invoke `args[0]` with the
/// remaining arguments (CPython's `tp_call` slot exposed as a wrapper).
fn slot_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let callee = args
        .first()
        .ok_or_else(|| type_error("__call__ needs an argument"))?;
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    // `type.__call__(cls, …)` is the *default* class call: it must not
    // re-dispatch through `type(cls).__call__`, or a metaclass
    // `__call__` that delegates to `type.__call__` recurses forever.
    if let Object::Type(ty) = callee {
        return interp.type_call_default(ty, &args[1..], kwargs);
    }
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(callee, &args[1..], kwargs, &globals)
}

/// Resolve the slot wrapper a *built-in* type `base_name` contributes for the
/// dunder `name`, or `None` if that type does not define it (so the caller's
/// MRO walk falls through to the next built-in base). Reuses the canonical
/// value-type implementations ([`unbound_method`]) and adds the universal
/// object-protocol dunders (`__repr__`/`__str__`/`__format__`) that aren't
/// modeled there.
///
/// `__str__` is intentionally restricted to the string-like built-ins; the
/// numeric/container types inherit `object.__str__` exactly as in CPython, so
/// `int.__str__ is object.__str__` holds and `IntEnum` correctly falls back to
/// `int.__repr__` for member stringification.
pub fn builtin_type_dunder(base_name: &str, name: &str) -> Option<Object> {
    // Memoised: repeated lookups must return the *same* object so
    // identity-based deduplication holds — enum's bootstrap compares
    // `getattr(cls, '__format__') in (member_type.__format__,
    // object.__format__)` to decide whether to substitute
    // `Enum.__format__`, which only works when `int.__format__` is one
    // stable object rather than a fresh wrapper per access.
    thread_local! {
        static DUNDER_CACHE: std::cell::RefCell<
            std::collections::HashMap<String, Option<Object>>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = format!("{base_name}.{name}");
    if let Some(hit) = DUNDER_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let computed = builtin_type_dunder_uncached(base_name, name);
    DUNDER_CACHE.with(|c| {
        c.borrow_mut().insert(key, computed.clone());
    });
    computed
}

fn builtin_type_dunder_uncached(base_name: &str, name: &str) -> Option<Object> {
    if let Some(o) = unbound_method(base_name, name) {
        return Some(o);
    }
    // `__call__` lives only on the callable types (CPython: `tp_call`
    // present on `type`, functions, methods, and the *callable* descriptor
    // types `method_descriptor`/`wrapper_descriptor` — but not on `object`,
    // nor on the data-only `getset_descriptor`/`member_descriptor`). The
    // descriptor types must carry it so `isinstance(list.append, Callable)`
    // holds (test_collections test_Callable).
    if name == "__call__"
        && matches!(
            base_name,
            "type"
                | "function"
                | "builtin_function_or_method"
                | "method"
                | "method-wrapper"
                | "method_descriptor"
                | "wrapper_descriptor"
        )
    {
        return Some(Object::Builtin(Rc::new(method_kw("__call__", slot_call))));
    }
    // `tp_str` is defined only by `object` and `str` among the value types
    // (CPython: `'__str__' in vars(int)` is False, hence
    // `int.__str__ is object.__str__` — identity the enum bootstrap's
    // `found_method in (data_type_method, object_method)` check relies
    // on). Other types fall through here so the caller's MRO walk
    // resolves `__str__` at `object`; exceptions get their own `__str__`
    // via type-dict entries installed at startup.
    if name == "__str__" {
        if base_name == "object" {
            return Some(Object::Builtin(Rc::new(method("__str__", slot_str))));
        }
        if base_name == "str" {
            // `str.__str__` is its own slot (`unicode_str`), distinct from
            // `object.__str__`: it returns the plain-`str` value of the
            // receiver *without* re-dispatching through `type(self).__str__`
            // or `__repr__`. StrEnum's `__str__ = str.__str__` relies on
            // this to yield the member's string payload.
            return Some(Object::Builtin(Rc::new(method("__str__", str_slot_str))));
        }
        return None;
    }
    // Rich-comparison slots are *object*'s defaults (identity `==`/`!=`,
    // `NotImplemented` orderings). The value types (str/int/float/bytes/
    // tuple/list/…) install their own value-based comparisons into their type
    // dict via `install_value_richcmp`. `builtin_slot_wrapper` walks the MRO
    // calling this helper *before* consulting each base's dict, so returning
    // object's identity slot for e.g. `str` shadowed str's real `__eq__` and
    // made `"c".__eq__("c")` decline with `NotImplemented` (instance-level
    // dunder access only — `str.__eq__` on the type still hit the dict). Only
    // surface these at `object`; every other base falls through so its own
    // dict entry wins (or it inherits object's later in the MRO walk).
    if matches!(
        name,
        "__eq__" | "__ne__" | "__lt__" | "__le__" | "__gt__" | "__ge__"
    ) && base_name != "object"
    {
        return None;
    }
    let (static_name, f): (&'static str, fn(&[Object]) -> Result<Object, RuntimeError>) = match name
    {
        "__repr__" => ("__repr__", slot_repr),
        "__format__" => ("__format__", slot_format),
        // `object`'s default rich comparisons: `==`/`!=` compare by
        // identity (value identity for primitives) and return
        // `NotImplemented` otherwise; the orderings are always
        // `NotImplemented` at the `object` level.
        "__eq__" => ("__eq__", slot_obj_eq),
        "__ne__" => ("__ne__", slot_obj_ne),
        "__lt__" => ("__lt__", slot_obj_ordering),
        "__le__" => ("__le__", slot_obj_ordering),
        "__gt__" => ("__gt__", slot_obj_ordering),
        "__ge__" => ("__ge__", slot_obj_ordering),
        "__dir__" => ("__dir__", b_dir),
        "__sizeof__" => ("__sizeof__", slot_sizeof),
        "__getstate__" => ("__getstate__", slot_getstate),
        _ => return None,
    };
    Some(Object::Builtin(Rc::new(method(static_name, f))))
}

/// Crate-visible handle on the native (non-dispatching) `__repr__` slot
/// so `type_surface` can materialize `int.__repr__`/`str.__repr__`/…
/// entries in the value-type dicts (CPython stores a `tp_repr` wrapper
/// per type; `enum._find_data_repr_` keys on `'__repr__' in
/// base.__dict__`).
pub(crate) fn value_slot_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    slot_repr(args)
}

/// `<builtin method>.__get__(obj, owner)` — bind a built-in method
/// descriptor to `obj` (CPython `method_get`). `args[0]` is the builtin
/// itself (the receiver of the bound `__get__`).
pub(crate) fn builtin_descriptor_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__get__() missing descriptor"))?;
    let instance = args.get(1).cloned().unwrap_or(Object::None);
    if matches!(instance, Object::None) {
        return Ok(func);
    }
    Ok(Object::BoundMethod(Rc::new(
        crate::object::BoundMethod::new(instance, func),
    )))
}

/// `method.__get__(self, obj, objtype=None)` — CPython gh-113157: a
/// bound `method` is *already* bound, so applying the descriptor
/// protocol to it again returns the method unchanged rather than
/// re-binding its `__func__` to a new receiver. `args[0]` is the
/// bound method itself (the `__get__` receiver).
pub(crate) fn method_descr_get(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("__get__() missing method"))
}

/// `str.__str__(self)` — CPython's `unicode_str`: return the receiver
/// itself when it is exactly `str`, or a plain-`str` copy of the native
/// payload for `str` subclasses. No virtual re-dispatch.
fn str_slot_str(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = args
        .first()
        .ok_or_else(|| type_error("__str__() takes exactly one argument (0 given)"))?;
    match o {
        Object::Str(_) => Ok(o.clone()),
        _ => match o.native_value() {
            Some(n @ Object::Str(_)) => Ok(n),
            _ => Err(type_error(format!(
                "descriptor '__str__' requires a 'str' object but received a '{}'",
                o.type_name()
            ))),
        },
    }
}

/// `object.__eq__(self, other)` — identity (payload equality for the
/// primitive value types), `NotImplemented` otherwise.
fn slot_obj_eq(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = match args {
        [a, b] => (a, b),
        _ => return Err(type_error("expected 2 arguments")),
    };
    if object_identity(a) == object_identity(b) {
        Ok(Object::Bool(true))
    } else {
        Ok(crate::vm_singletons::not_implemented())
    }
}

/// `object.__ne__(self, other)` — the negation of `__eq__`, staying
/// `NotImplemented` when equality is undecided.
fn slot_obj_ne(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = match args {
        [a, b] => (a, b),
        _ => return Err(type_error("expected 2 arguments")),
    };
    if object_identity(a) == object_identity(b) {
        Ok(Object::Bool(false))
    } else {
        Ok(crate::vm_singletons::not_implemented())
    }
}

/// `object.__lt__` / `__le__` / `__gt__` / `__ge__` — `object` defines
/// no ordering: always `NotImplemented`.
fn slot_obj_ordering(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::vm_singletons::not_implemented())
}

/// `object.__sizeof__(self)` — a coarse byte size. WeavePy objects
/// don't share CPython's memory layout; report the CPython-typical
/// fixed header size so the protocol surface exists and returns a
/// plausible positive int.
fn slot_sizeof(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = one(args, "__sizeof__")?;
    let size: i64 = match o {
        // CPython's `int.__sizeof__`: `int.__basicsize__ +
        // int.__itemsize__ * ndigits` with 30-bit digits and at least one
        // digit even for zero (test_long.test___sizeof__ asserts the exact
        // formula, including for int subclasses via the fallthrough below).
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => {
            let bits = o.as_bigint().expect("int-like").bits();
            let ndigits = (bits.max(1)).div_ceil(30) as i64;
            28 + 4 * ndigits
        }
        Object::Instance(inst) => {
            if let Some(native) = inst.native.get() {
                if native.is_int_like() {
                    let bits = native.as_bigint().expect("int-like").bits();
                    let ndigits = (bits.max(1)).div_ceil(30) as i64;
                    return Ok(Object::Int(28 + 4 * ndigits));
                }
            }
            16 + 8 * inst.dict.borrow().len() as i64
        }
        // CPython's compact-unicode layout (test_str.test_raiseMemError):
        // ASCII is a 40-byte struct + len+1 one-byte units; anything wider
        // is 56 bytes + (len+1) units of the kind width (1 for latin-1,
        // 2 for BMP, 4 beyond).
        Object::Str(s) => {
            let len = str_char_len(s) as i64;
            let max_cp = s.chars().map(u32::from).max().unwrap_or(0);
            match max_cp {
                0..=0x7f => 40 + len + 1,
                0x80..=0xff => 56 + (len + 1),
                0x100..=0xffff => 56 + 2 * (len + 1),
                _ => 56 + 4 * (len + 1),
            }
        }
        Object::WStr(cps) => {
            let len = cps.len() as i64;
            let max_cp = cps.iter().copied().max().unwrap_or(0);
            match max_cp {
                0..=0x7f => 40 + len + 1,
                0x80..=0xff => 56 + (len + 1),
                0x100..=0xffff => 56 + 2 * (len + 1),
                _ => 56 + 4 * (len + 1),
            }
        }
        Object::Bytes(b) => 33 + b.len() as i64,
        Object::List(items) => 56 + 8 * items.borrow().len() as i64,
        Object::Tuple(items) => 40 + 8 * items.len() as i64,
        Object::Dict(d) => 64 + 24 * d.borrow().len() as i64,
        _ => 16,
    };
    Ok(Object::Int(size))
}

/// `object.__getstate__(self)` — PEP 307 default pickling state: the
/// instance `__dict__` when non-empty, else `None`. When `__slots__`
/// values are populated, CPython returns the 2-tuple
/// `(dict_or_None, {slot: value, …})` instead.
fn slot_getstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let o = one(args, "__getstate__")?;
    if let Object::Instance(inst) = o {
        let slots = inst.slots_snapshot();
        let dict_is_empty = inst.dict.borrow().is_empty();
        let dict_state = if dict_is_empty {
            Object::None
        } else {
            Object::Dict(inst.dict.clone())
        };
        if !slots.is_empty() {
            let mut slot_dict = crate::object::DictData::default();
            for (name, value) in slots {
                slot_dict.insert(DictKey(Object::from_str(name)), value);
            }
            return Ok(Object::new_tuple(vec![
                dict_state,
                Object::Dict(Rc::new(RefCell::new(slot_dict))),
            ]));
        }
        return Ok(dict_state);
    }
    Ok(Object::None)
}

// ---------- free builtins ----------

fn one<'a>(args: &'a [Object], name: &str) -> Result<&'a Object, RuntimeError> {
    args.first()
        .ok_or_else(|| type_error(format!("{name}() takes 1 argument (0 given)")))
}

fn b_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "len")?;
    Ok(Object::Int(v.len()? as i64))
}

/// Coerce a `list.index`/`tuple.index` start/stop bound to `i64`, clamping
/// an out-of-range big integer to `i64::MAX`/`i64::MIN` the way CPython's
/// `_PyEval_SliceIndex` saturates a `Py_ssize_t` — so `index(x, 4*sys.maxsize)`
/// is an empty window, not an `OverflowError`.
pub(crate) fn seq_index_bound(o: &Object) -> Result<i64, RuntimeError> {
    if let Object::Long(b) = o {
        if let Some(v) = b.to_i64() {
            return Ok(v);
        }
        return Ok(if b.is_negative() { i64::MIN } else { i64::MAX });
    }
    coerce_index_i64(o)
}

pub(crate) fn coerce_index_i64(o: &Object) -> Result<i64, RuntimeError> {
    if let Some(res) = try_coerce_index_i64(o) {
        return res;
    }
    Err(type_error(format!(
        "'{}' object cannot be interpreted as an integer",
        o.type_name()
    )))
}

/// CPython's `PyNumber_Index` without the C-ssize_t narrowing: coerce `o`
/// through `__index__` and return the resulting int *object* (Int or Long)
/// at full width. `hex()`/`oct()`/`bin()` accept any magnitude — a
/// `np.uint64` above `i64::MAX` must format, not raise `OverflowError`.
pub(crate) fn coerce_index_object(o: &Object) -> Result<Object, RuntimeError> {
    match o {
        Object::Int(_) | Object::Long(_) => return Ok(o.clone()),
        Object::Bool(b) => return Ok(Object::Int(i64::from(*b))),
        _ => {}
    }
    if !matches!(o, Object::Instance(_) | Object::Foreign(_)) {
        return Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            o.type_name()
        )));
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            o.type_name()
        ))
    })?;
    // SAFETY: published by an enclosing VM frame still live on this thread.
    let interp = unsafe { &mut *ptr };
    let Ok(method) = interp.load_attr_public(o, "__index__") else {
        return Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            o.type_name()
        )));
    };
    let globals = interp.builtins_dict();
    let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
    match r {
        Object::Int(_) | Object::Long(_) => Ok(r),
        Object::Bool(b) => Ok(Object::Int(i64::from(b))),
        other => Err(type_error(format!(
            "__index__ returned non-int (type {})",
            other.type_name()
        ))),
    }
}

/// Like [`coerce_index_i64`], but distinguishes "has no `__index__`" (→
/// `None`, so a caller can raise a context-specific message such as "tuple
/// indices must be integers") from "has `__index__`, here is its (possibly
/// failing) result" (→ `Some(..)`).
///
/// Unlike the old `instance_method`-only lookup, this resolves `__index__`
/// through full attribute resolution, so a **C-slot** `nb_index` reached only
/// via the bridge — a `numpy` integer scalar (`np.intp`, used to index
/// `BlockManager.blocks[blknos[i]]`) — is honoured, matching CPython's
/// `PyNumber_Index`.
pub(crate) fn try_coerce_index_i64(o: &Object) -> Option<Result<i64, RuntimeError>> {
    if let Some(v) = o.as_i64() {
        return Some(Ok(v));
    }
    // A big integer is a valid `__index__` value, but it can't fit the C
    // ssize_t the caller wants → `OverflowError`, matching CPython
    // (`test_io.test_reconfigure_errors`: `line_buffering=2**1000`).
    if matches!(o, Object::Long(_)) {
        return Some(Err(crate::error::overflow_error(
            "cannot fit 'int' into an index-sized integer",
        )));
    }
    if !matches!(o, Object::Instance(_) | Object::Foreign(_)) {
        return None;
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: the pointer was published by an enclosing VM frame still live on
    // this thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let Ok(method) = interp.load_attr_public(o, "__index__") else {
        return None;
    };
    let globals = interp.builtins_dict();
    Some((|| {
        let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
        if let Some(v) = r.as_i64() {
            return Ok(v);
        }
        // `__index__` returned an int too large for an index-sized C integer
        // (CPython raises `OverflowError`, not `TypeError`).
        if matches!(r, Object::Long(_)) {
            return Err(crate::error::overflow_error(
                "cannot fit 'int' into an index-sized integer",
            ));
        }
        Err(type_error(format!(
            "__index__ returned non-int (type {})",
            r.type_name()
        )))
    })())
}

/// Coerce `o` to an `f64` the way CPython's float-accepting C functions
/// (`math.*`, etc.) do: floats/ints/bools/big ints directly, built-in
/// numeric subclass payloads by unwrapping, and otherwise via the Python
/// `__float__` then `__index__` protocol through interpreter reentry.
///
/// `Ok(None)` means "not coercible" — the caller raises its own
/// function-specific `TypeError`. `Err` propagates an exception raised
/// inside a user `__float__`/`__index__`.
pub(crate) fn coerce_f64_opt(o: &Object) -> Result<Option<f64>, RuntimeError> {
    match o {
        Object::Float(f) => Ok(Some(*f)),
        Object::Int(i) => Ok(Some(*i as f64)),
        Object::Bool(b) => Ok(Some(if *b { 1.0 } else { 0.0 })),
        Object::Long(b) => {
            use num_traits::ToPrimitive;
            // CPython's `float(int)` (PyLong_AsDouble) raises OverflowError when
            // the magnitude exceeds the finite double range, rather than
            // silently yielding inf — `math.dist((1, 10**313), …)` relies on
            // this to reject coordinates that can't be represented.
            match b.to_f64() {
                Some(f) if f.is_finite() => Ok(Some(f)),
                _ => Err(crate::error::overflow_error(
                    "int too large to convert to float",
                )),
            }
        }
        Object::Instance(inst) => {
            if let Some(native) = inst.native.get() {
                let native = native.clone();
                return coerce_f64_opt(&native);
            }
            for dunder in ["__float__", "__index__"] {
                if let Some(method) = crate::instance_method(o, dunder) {
                    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                        // SAFETY: published by an enclosing VM frame still live
                        // on this thread; the GIL keeps the access exclusive.
                        let interp = unsafe { &mut *ptr };
                        let globals = interp.builtins_dict();
                        let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                        return coerce_f64_opt(&r);
                    }
                }
            }
            Ok(None)
        }
        Object::Foreign(s) => {
            // A foreign extension scalar (numpy `float64`/`int64`/…) exposes
            // its value through the binary-ABI `nb_float`/`nb_index` slots,
            // exactly as `float(x)` consumes it (`do_float_call`). Without this
            // `math.isclose`/`math.dist`/`statistics.*` rejected `float64`
            // operands ("must be a real number, not 'object'") that pandas'
            // `assert_almost_equal`, `Series.cov`/`corr`, etc. feed them —
            // CPython's `PyFloat_AsDouble` honours `nb_float` then `nb_index`.
            match crate::foreign::as_float(s) {
                Ok(v) => coerce_f64_opt(&v),
                Err(_) => match crate::foreign::as_index(s) {
                    Ok(v) => coerce_f64_opt(&v),
                    Err(_) => Ok(None),
                },
            }
        }
        _ => Ok(None),
    }
}

/// `__index__`-coerce a `range()` bound at full precision — CPython's
/// range constructor takes arbitrary ints (`range(2**200, 2**201)`,
/// test_range test_comparison/test_large_range).
fn coerce_index_bigint(o: &Object) -> Result<BigInt, RuntimeError> {
    match o {
        Object::Bool(b) => Ok(BigInt::from(i64::from(*b))),
        Object::Int(i) => Ok(BigInt::from(*i)),
        Object::Long(b) => Ok((**b).clone()),
        Object::Instance(_) | Object::Foreign(_) => {
            if let Some(v) = o.as_i64() {
                return Ok(BigInt::from(v));
            }
            let r = coerce_index_object(o)?;
            coerce_index_bigint(&r)
        }
        _ => coerce_index_i64(o).map(BigInt::from),
    }
}

fn b_range(args: &[Object]) -> Result<Object, RuntimeError> {
    let to_int = coerce_index_bigint;
    let (start, stop, step) = match args.len() {
        1 => (BigInt::from(0), to_int(&args[0])?, BigInt::from(1)),
        2 => (to_int(&args[0])?, to_int(&args[1])?, BigInt::from(1)),
        3 => (to_int(&args[0])?, to_int(&args[1])?, to_int(&args[2])?),
        0 => return Err(type_error("range expected at least 1 argument, got 0")),
        n => {
            return Err(type_error(format!(
                "range expected at most 3 arguments, got {n}"
            )))
        }
    };
    if step == BigInt::from(0) {
        return Err(value_error("range() arg 3 must not be zero"));
    }
    Ok(Object::Range(Rc::new(Range::from_bigints(
        start, stop, step,
    ))))
}

/// PEP 0467 int→str conversion cap. Raises `ValueError` when the decimal
/// expansion of `b` would exceed `sys.get_int_max_str_digits()` (0 = off).
///
/// The expensive base-10 conversion is avoided for pathological inputs: the
/// digit count is first bounded from the bit length, and the exact string is
/// only materialised when the magnitude sits right at the limit (in which
/// case it is small and cheap to convert).
pub(crate) fn long_str_limit_check(b: &num_bigint::BigInt) -> Result<(), RuntimeError> {
    let max_digits = crate::stdlib::sys::int_max_str_digits();
    if max_digits <= 0 {
        return Ok(());
    }
    let limit = max_digits as u64;
    let bits = b.bits();
    if bits == 0 {
        return Ok(()); // "0" — a single digit, never exceeds the (>=640) cap.
    }
    const LOG10_2: f64 = std::f64::consts::LOG10_2;
    let lower = (((bits - 1) as f64) * LOG10_2).floor() as u64 + 1;
    if lower > limit {
        return Err(int_to_str_limit_error(max_digits));
    }
    let upper = ((bits as f64) * LOG10_2).floor() as u64 + 1;
    if upper <= limit {
        return Ok(());
    }
    // Boundary case: the value is within ~1 digit of the cap, so it is small
    // enough to expand exactly without performance risk.
    if b.magnitude().to_str_radix(10).len() as u64 > limit {
        return Err(int_to_str_limit_error(max_digits));
    }
    Ok(())
}

fn int_to_str_limit_error(max_digits: i64) -> RuntimeError {
    value_error(format!(
        "Exceeds the limit ({max_digits} digits) for integer string conversion; \
         use sys.set_int_max_str_digits() to increase the limit"
    ))
}

fn b_str(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::from_static(""));
    }
    if args.len() > 3 {
        return Err(type_error(format!(
            "str expected at most 3 arguments, got {}",
            args.len()
        )));
    }
    if let Object::Long(b) = &args[0] {
        long_str_limit_check(b)?;
    }
    // `str(object, encoding[, errors])` decodes a bytes-like object,
    // equivalent to `object.decode(encoding, errors)`. CPython's
    // `re._parser.Tokenizer` relies on `str(pattern, 'latin1')` to
    // tokenize bytes patterns, so this path must decode rather than
    // fall back to `repr`-style stringification.
    if args.len() >= 2 {
        // The clinic parser validates the `encoding`/`errors` *types* before
        // the decode step complains about the object (`str(1, 1)` is the
        // encoding TypeError, not "need a bytes-like object" —
        // test_str.test_str_invalid_call).
        let encoding = match &args[1] {
            Object::Str(e) => e.to_string(),
            Object::None => "utf-8".to_owned(),
            other => {
                return Err(type_error(format!(
                    "str() argument 'encoding' must be str, not {}",
                    other.type_name()
                )))
            }
        };
        let errors = match args.get(2) {
            Some(Object::Str(e)) => e.to_string(),
            Some(Object::None) | None => "strict".to_owned(),
            Some(other) => {
                return Err(type_error(format!(
                    "str() argument 'errors' must be str, not {}",
                    other.type_name()
                )))
            }
        };
        // Any buffer decodes (`str(memoryview(b'…'), 'utf-8')`,
        // test_str.test_constructor).
        let data = match args[0].as_bytes_view() {
            Some(v) => v,
            None => {
                return Err(type_error(format!(
                    "decoding to str: need a bytes-like object, {} found",
                    args[0].type_name()
                )));
            }
        };
        return crate::stdlib::codecs_mod::decode_bytes_obj(&data, &encoding, &errors);
    }
    // Identity for strings — a `WStr` in particular must keep its lone
    // surrogates rather than flatten to U+FFFD through `to_str()`.
    if matches!(&args[0], Object::Str(_) | Object::WStr(_)) {
        return Ok(args[0].clone());
    }
    // Dispatch `__str__` virtually when a VM is live — the subclass
    // constructor reaches `b_str` directly (not through the interpreter's
    // `str` interception), and `StrSubclass(WithStr('abc'))` must convert
    // through `WithStr.__str__`, not the `repr` fallback
    // (test_str.test_conversion).
    if matches!(
        &args[0],
        Object::Instance(_) | Object::Type(_) | Object::Foreign(_)
    ) {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on this
            // thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let s = interp.stringify_public(&args[0], &globals)?;
            return Ok(bridge_to_object(&s));
        }
    }
    Ok(Object::from_str(args[0].to_str()))
}

/// Keyword form of `str()` — CPython's clinic signature is
/// `str(object='', encoding=..., errors=...)`: when `encoding` or `errors`
/// is supplied the object defaults to `b''` and is *decoded*
/// (`str(errors='strict')` is `''`, test_str.test_constructor_defaults);
/// name/position collisions and unknown keywords use the clinic wording.
fn b_str_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if kwargs.is_empty() {
        return b_str(args);
    }
    for (k, _) in kwargs {
        if !matches!(k.as_str(), "object" | "encoding" | "errors") {
            return Err(type_error(format!(
                "str() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    let total = args.len() + kwargs.len();
    if total > 3 {
        return Err(type_error(format!(
            "str() takes at most 3 arguments ({total} given)"
        )));
    }
    let mut object = args.first().cloned();
    let mut encoding = args.get(1).cloned();
    let mut errors = args.get(2).cloned();
    for (k, v) in kwargs {
        let (slot, pos) = match k.as_str() {
            "object" => (&mut object, 1),
            "encoding" => (&mut encoding, 2),
            _ => (&mut errors, 3),
        };
        if slot.is_some() {
            return Err(type_error(format!(
                "argument for str() given by name ('{k}') and position ({pos})"
            )));
        }
        *slot = Some(v.clone());
    }
    if encoding.is_none() && errors.is_none() {
        return match object {
            Some(o) => b_str(&[o]),
            None => Ok(Object::from_static("")),
        };
    }
    let object = object.unwrap_or_else(|| Object::new_bytes(Vec::new()));
    let mut positional = vec![object, encoding.unwrap_or(Object::None)];
    if let Some(e) = errors {
        positional.push(e);
    }
    b_str(&positional)
}

fn b_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "repr")?;
    if let Object::Long(b) = v {
        long_str_limit_check(b)?;
    }
    Ok(Object::from_str(v.repr()))
}

fn b_format(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Err(type_error("format() expects at least 1 argument"));
    }
    if args.len() > 2 {
        return Err(type_error("format() takes at most 2 arguments"));
    }
    let value = &args[0];
    let spec = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Object::from_static(""));
    let spec_str = match &spec {
        Object::Str(s) => s.to_string(),
        _ => return Err(type_error("format() spec must be a string")),
    };
    let s = crate::format_via_spec(value, &spec_str)?;
    Ok(Object::from_str(s))
}

fn b_ascii(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "ascii")?;
    Ok(Object::from_str(crate::ascii_value(v)))
}

/// `property(fget, fset=None, fdel=None, doc=None)`. Returns a real
/// data descriptor; the VM dispatches `__get__` / `__set__` /
/// `__delete__` on attribute access (see `Vm::descriptor_get` and
/// the data-descriptor branch in `Vm::store_attr` /
/// `Vm::delete_attr`).
pub fn construct_property(args: &[Object]) -> Result<Object, RuntimeError> {
    let fget = args.first().cloned().unwrap_or(Object::None);
    let fset = args.get(1).cloned().unwrap_or(Object::None);
    let fdel = args.get(2).cloned().unwrap_or(Object::None);
    let doc = args.get(3).cloned().unwrap_or(Object::None);
    let prop = Rc::new(crate::object::PyProperty::new(
        Object::None,
        Object::None,
        Object::None,
        Object::None,
    ));
    property_init_members(&prop, None, fget, fset, fdel, doc)?;
    Ok(Object::Property(prop))
}

/// The `PyProperty` payload behind a receiver: the value itself for an
/// exact `property`, the wrapped native payload for a `property`
/// subclass instance. `None` for anything else.
pub(crate) fn property_payload(recv: &Object) -> Option<Rc<crate::object::PyProperty>> {
    match recv {
        Object::Property(p) => Some(p.clone()),
        Object::Instance(i) => {
            if let Some(Object::Property(p)) = i.native.get() {
                return Some(p.clone());
            }
            // A property-subclass instance allocated without the payload —
            // e.g. a raw `property.__new__(Sub)` that never went through
            // `instantiate`'s native-payload path. CPython's allocation
            // always carries the C property struct, so attach an empty one
            // lazily (gh-100942 exercises exactly this shape).
            if i.native.get().is_none()
                && i.cls()
                    .is_subclass_of(&crate::builtin_types::builtin_types().property_)
            {
                let _ = i
                    .native
                    .set(Object::Property(Rc::new(crate::object::PyProperty::new(
                        Object::None,
                        Object::None,
                        Object::None,
                        Object::None,
                    ))));
                if let Some(Object::Property(p)) = i.native.get() {
                    return Some(p.clone());
                }
            }
            None
        }
        _ => None,
    }
}

/// CPython `property_init_impl`'s subclass branch, run right after
/// `instantiate` builds a property-subclass instance: the doc computed by
/// the exact-type constructor moves from the native payload onto the
/// *instance* (`__dict__` or a `__doc__` slot), so the subclass's own
/// class docstring cannot shadow it (issue 41287). A write failing with
/// AttributeError (dict-less `__slots__` subclass) is tolerated
/// (gh-98963) unless the doc came from the getter, whose failure
/// historically surfaces (test_slots_docstring_copy_exception).
pub(crate) fn property_relocate_subclass_doc(inst: &Object) -> Result<(), RuntimeError> {
    let Some(prop) = property_payload(inst) else {
        return Ok(());
    };
    let doc = prop.doc();
    *prop.doc.borrow_mut() = Object::None;
    let getter_doc = prop.getter_doc.get();
    match reentrant_store_attr(inst, "__doc__", doc) {
        Ok(()) => Ok(()),
        Err(e) if !getter_doc && is_attribute_error_reentrant(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

/// CPython's unreachable-property error family — `property 'x' of 'C'
/// object has no getter/setter/deleter` — with the name segment present
/// only when `__set_name__` recorded one, and `C` being the type's
/// *qualified* name (test_property `_PropertyUnreachableAttribute`).
pub(crate) fn property_unreachable_error(
    prop: &crate::object::PyProperty,
    receiver: &Object,
    verb: &str,
) -> RuntimeError {
    let cls = class_of(receiver);
    let qual = cls
        .qualname
        .borrow()
        .clone()
        .unwrap_or_else(|| cls.name.clone());
    crate::error::attribute_error(match &*prop.name.borrow() {
        Some(n) => format!("property {} of '{qual}' object has no {verb}", n.repr()),
        None => format!("property of '{qual}' object has no {verb}"),
    })
}

/// Whether `e` is an `AttributeError`, judged by the running interpreter
/// when one is live (so subclasses match too).
fn is_attribute_error_reentrant(e: &RuntimeError) -> bool {
    match crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame live on this thread.
        Some(ptr) => unsafe { &*ptr }.is_attribute_error(e),
        None => false,
    }
}

/// Optional-attribute lookup through the running interpreter (so dynamic
/// `__doc__`/`__name__` descriptors dispatch); `Ok(None)` for a missing
/// attribute, mirroring `PyObject_GetOptionalAttr`.
fn reentrant_load_attr_opt(obj: &Object, name: &str) -> Result<Option<Object>, RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(attr_get(obj, name));
    };
    // SAFETY: published by an enclosing VM frame live on this thread.
    let interp = unsafe { &mut *ptr };
    match interp.load_attr(obj, name) {
        Ok(v) => Ok(Some(v)),
        Err(e) if interp.is_attribute_error(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `setattr(obj, name, value)` through the running interpreter.
fn reentrant_store_attr(obj: &Object, name: &str, value: Object) -> Result<(), RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: published by an enclosing VM frame live on this thread.
    let interp = unsafe { &mut *ptr };
    interp.store_attr(obj, name, value)
}

/// CPython `property_init_impl`: install the accessors, then compute the
/// docstring. An explicit non-None `doc` wins; otherwise the getter's
/// `__doc__` is harvested, with `getter_doc` recording that provenance
/// (it controls what `property_copy` carries over). For a property
/// *subclass* instance the doc is stored on the instance (`__dict__` or a
/// `__doc__` slot) rather than the native payload, so the subclass's own
/// class docstring cannot shadow it (issue 41287); a write failing with
/// AttributeError is tolerated (gh-98963) *except* when the doc came from
/// the getter, whose failure historically surfaces
/// (test_slots_docstring_copy_exception).
fn property_init_members(
    prop: &crate::object::PyProperty,
    subclass_receiver: Option<&Object>,
    fget: Object,
    fset: Object,
    fdel: Object,
    doc: Object,
) -> Result<(), RuntimeError> {
    prop.reinit(fget, fset, fdel, Object::None);
    let mut prop_doc = Object::None;
    let mut getter_doc = false;
    if !matches!(doc, Object::None) {
        prop_doc = doc;
    } else {
        let fget = prop.fget();
        if !matches!(fget, Object::None) {
            if let Some(d) = reentrant_load_attr_opt(&fget, "__doc__")? {
                if !matches!(d, Object::None) {
                    prop_doc = d;
                    getter_doc = true;
                }
            }
        }
    }
    prop.getter_doc.set(getter_doc);
    match subclass_receiver {
        None => {
            *prop.doc.borrow_mut() = prop_doc;
        }
        Some(recv) => {
            // The payload's own doc stays None; reads resolve through the
            // instance attribute, mirroring CPython's subclass branch.
            match reentrant_store_attr(recv, "__doc__", prop_doc) {
                Ok(()) => {}
                Err(e) if !getter_doc && is_attribute_error_reentrant(&e) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// `staticmethod(f)` — non-data descriptor that returns the wrapped
/// callable unchanged on access.
pub fn construct_staticmethod(args: &[Object]) -> Result<Object, RuntimeError> {
    let inner = args.first().cloned().unwrap_or(Object::None);
    Ok(Object::StaticMethod(MethodWrapper::new(inner)))
}

/// `classmethod(f)` — non-data descriptor that binds the wrapped
/// callable to the *class* (not the instance) on access.
pub fn construct_classmethod(args: &[Object]) -> Result<Object, RuntimeError> {
    let inner = args.first().cloned().unwrap_or(Object::None);
    Ok(Object::ClassMethod(MethodWrapper::new(inner)))
}

/// Set the wrapped callable on a `staticmethod`/`classmethod`
/// (sub)instance — CPython's `sm_init`/`cm_init`. `__new__` builds the
/// wrapper with `__func__ == None`; this fills it in. A subclass that
/// overrides `__init__` without chaining to `super().__init__` never
/// reaches here, so its `__func__` stays `None` (test_descr
/// `test_classmethod_new` / `test_staticmethod_new`).
fn method_wrapper_set_func(args: &[Object]) {
    let func = args.get(1).cloned().unwrap_or(Object::None);
    match args.first() {
        Some(Object::StaticMethod(w) | Object::ClassMethod(w)) => w.set_func(func),
        Some(Object::Instance(i)) => {
            if let Some(Object::StaticMethod(w) | Object::ClassMethod(w)) = i.native.get() {
                w.set_func(func);
            }
        }
        _ => {}
    }
}

/// `staticmethod.__init__(self, func)` — CPython's `sm_init`.
pub(crate) fn staticmethod_init(args: &[Object]) -> Result<Object, RuntimeError> {
    method_wrapper_set_func(args);
    Ok(Object::None)
}

/// `classmethod.__init__(self, func)` — CPython's `cm_init`.
pub(crate) fn classmethod_init(args: &[Object]) -> Result<Object, RuntimeError> {
    method_wrapper_set_func(args);
    Ok(Object::None)
}

/// `staticmethod.__get__(self, obj, objtype=None)` — the descriptor hook.
/// A staticmethod ignores the binding context and hands back the wrapped
/// callable unchanged (matching CPython's `sm_descr_get`). Exposing it as
/// a real method lets descriptor-aware code — notably
/// `functools.partialmethod`, which does `self.func.__get__(obj, cls)` —
/// treat a wrapped `staticmethod` correctly. `args[0]` is the descriptor
/// itself (the bound receiver).
pub(crate) fn staticmethod_descr_get(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::StaticMethod(inner)) => Ok(inner.func()),
        // Tolerate an already-unwrapped callable (defensive).
        Some(other) => Ok(other.clone()),
        None => Err(type_error("staticmethod.__get__() missing self")),
    }
}

/// `classmethod.__get__(self, obj, objtype=None)` — binds the wrapped
/// callable to the owning *class* and returns a bound method (CPython's
/// `cm_descr_get`). The owner is the explicit `objtype` when supplied,
/// otherwise `type(obj)`.
pub(crate) fn classmethod_descr_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inner = match args.first() {
        Some(Object::ClassMethod(i)) => i.func(),
        _ => return Err(type_error("classmethod.__get__() missing self")),
    };
    let owner = match args.get(2) {
        Some(o) if !matches!(o, Object::None) => o.clone(),
        _ => match args.get(1) {
            Some(o) if !matches!(o, Object::None) => Object::Type(class_of(o)),
            _ => return Err(type_error("classmethod.__get__(None, None) is not valid")),
        },
    };
    Ok(Object::BoundMethod(Rc::new(
        crate::object::BoundMethod::new(owner, inner),
    )))
}

/// `function.__get__(self, obj, objtype=None)` — a plain Python function
/// is a non-data descriptor: bound to an instance it yields a bound
/// method, bound to `None` (class access) it returns the function itself
/// (CPython's `func_descr_get`). Exposing it makes functions usable with
/// descriptor-aware library code such as `functools.partialmethod`.
pub(crate) fn function_descr_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let func = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__get__() missing self"))?;
    match args.get(1) {
        Some(obj) if !matches!(obj, Object::None) => Ok(Object::BoundMethod(Rc::new(
            crate::object::BoundMethod::new(obj.clone(), func),
        ))),
        _ => Ok(func),
    }
}

/// Build the callable `Object::Builtin` backing `staticmethod.__get__` /
/// `classmethod.__get__`. The VM wires this into a `BoundMethod` whose
/// receiver is the descriptor object, so `args[0]` arrives as the
/// descriptor when the hook runs.
pub(crate) fn descriptor_get_builtin(is_static: bool) -> Object {
    let f = if is_static {
        method("__get__", staticmethod_descr_get)
    } else {
        method("__get__", classmethod_descr_get)
    };
    Object::Builtin(Rc::new(f))
}

/// Build the callable `Object::Builtin` backing `function.__get__`.
pub(crate) fn function_get_builtin() -> Object {
    Object::Builtin(Rc::new(method("__get__", function_descr_get)))
}

/// CPython `property_copy` (descrobject.c): `p.getter(f)` / `setter` /
/// `deleter` build a *new* descriptor by calling `type(p)(get, set, del,
/// doc)` — preserving property subclasses — and carry the
/// `__set_name__`-recorded name over when the result really is a
/// property (gh-100942: a subclass `__new__` may return anything, which
/// must not be treated as a property).
fn property_with(
    args: &[Object],
    which: crate::object::PropertyAttr,
) -> Result<Object, RuntimeError> {
    use crate::object::PropertyAttr;
    let recv = args.first().cloned().unwrap_or(Object::None);
    let prop =
        property_payload(&recv).ok_or_else(|| type_error("expected property as first argument"))?;
    let new_fn = args.get(1).cloned().unwrap_or(Object::None);
    // A None replacement keeps the old accessor (CPython treats NULL and
    // Py_None alike in `property_copy`).
    let pick = |old: Object, mine: bool| {
        if mine && !matches!(new_fn, Object::None) {
            new_fn.clone()
        } else {
            old
        }
    };
    let get = pick(prop.fget(), which == PropertyAttr::Get);
    let set = pick(prop.fset(), which == PropertyAttr::Set);
    let del = pick(prop.fdel(), which == PropertyAttr::Del);
    // A getter-derived doc is dropped so the init re-harvests it from the
    // (possibly new) getter; an explicit doc is carried over verbatim.
    let doc = if prop.getter_doc.get() && !matches!(get, Object::None) {
        Object::None
    } else {
        prop.doc()
    };
    let copied = match &recv {
        // Subclass instance: call the subclass type, running its own
        // `__new__`/`__init__` chain.
        Object::Instance(i) => reentrant_call(&Object::Type(i.cls()), &[get, set, del, doc])?,
        _ => construct_property(&[get, set, del, doc])?,
    };
    if let Some(new_prop) = property_payload(&copied) {
        *new_prop.name.borrow_mut() = prop.name.borrow().clone();
    }
    Ok(copied)
}

fn property_getter(args: &[Object]) -> Result<Object, RuntimeError> {
    property_with(args, crate::object::PropertyAttr::Get)
}

fn property_setter(args: &[Object]) -> Result<Object, RuntimeError> {
    property_with(args, crate::object::PropertyAttr::Set)
}

fn property_deleter(args: &[Object]) -> Result<Object, RuntimeError> {
    property_with(args, crate::object::PropertyAttr::Del)
}

/// Re-enter the running interpreter to call a Python-level callable from
/// builtin context. Shared by the explicit descriptor-protocol slots
/// (`property.__get__` / `__set__` / `__delete__`), whose accessors are
/// ordinary Python functions.
/// `str(obj)` through the running interpreter (so user `__str__` /
/// nested-exception rendering dispatches). `None` when no interpreter
/// is live on this thread.
pub(crate) fn str_reentrant(obj: &Object) -> Option<String> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: the pointer was published by an enclosing VM frame still
    // live on this thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    interp.stringify_public(obj, &globals).ok()
}

pub(crate) fn reentrant_call(callable: &Object, args: &[Object]) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: the pointer was published by an enclosing VM frame still
    // live on this thread; the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(callable, args, &[], &globals)
}

fn property_self(args: &[Object], op: &str) -> Result<Rc<crate::object::PyProperty>, RuntimeError> {
    args.first()
        .and_then(property_payload)
        .ok_or_else(|| type_error(format!("descriptor '{op}' requires a 'property' object")))
}

/// `property.__init__(self, fget=None, fset=None, fdel=None, doc=None)`
/// — CPython's `property_init`: replaces all four members on the
/// *existing* descriptor. Reached by subclass `__init__` chains
/// (`super().__init__(fget, doc=doc)`); the receiver may be the raw
/// payload (super's native-payload probe) or the wrapping instance.
fn property_init_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__init__() missing self"))?;
    let prop = property_payload(recv)
        .ok_or_else(|| type_error("descriptor '__init__' requires a 'property' object"))?;
    let mut members: [Object; 4] = [
        args.get(1).cloned().unwrap_or(Object::None),
        args.get(2).cloned().unwrap_or(Object::None),
        args.get(3).cloned().unwrap_or(Object::None),
        args.get(4).cloned().unwrap_or(Object::None),
    ];
    if args.len() > 5 {
        return Err(type_error(format!(
            "property() takes at most 4 arguments ({} given)",
            args.len() - 1
        )));
    }
    for (k, v) in kwargs {
        let idx = match k.as_str() {
            "fget" => 0,
            "fset" => 1,
            "fdel" => 2,
            "doc" => 3,
            other => {
                return Err(type_error(format!(
                    "property() got an unexpected keyword argument '{other}'"
                )))
            }
        };
        if idx + 1 < args.len() {
            return Err(type_error(format!(
                "argument for property() given by name ('{k}') and position ({})",
                idx + 1
            )));
        }
        members[idx] = v.clone();
    }
    let [fget, fset, fdel, doc] = members;
    let subclass_receiver = match recv {
        Object::Instance(_) => Some(recv),
        _ => None,
    };
    property_init_members(&prop, subclass_receiver, fget, fset, fdel, doc)?;
    Ok(Object::None)
}

/// `property.__get__(self, obj, objtype=None)` — CPython's
/// `property_descr_get`: class access (obj is None) returns the property
/// itself; instance access invokes `fget`.
fn property_dunder_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = property_self(args, "__get__")?;
    match args.get(1) {
        Some(obj) if !matches!(obj, Object::None) => {
            let fget = p.fget();
            if matches!(fget, Object::None) {
                return Err(property_unreachable_error(&p, obj, "getter"));
            }
            reentrant_call(&fget, &[obj.clone()])
        }
        _ => Ok(args[0].clone()),
    }
}

/// `property.__set__(self, obj, value)` — CPython's `property_descr_set`.
fn property_dunder_set(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = property_self(args, "__set__")?;
    let (obj, value) = match (args.get(1), args.get(2)) {
        (Some(o), Some(v)) => (o.clone(), v.clone()),
        _ => return Err(type_error("__set__() takes exactly 3 arguments")),
    };
    let fset = p.fset();
    if matches!(fset, Object::None) {
        return Err(property_unreachable_error(&p, &obj, "setter"));
    }
    reentrant_call(&fset, &[obj, value])?;
    Ok(Object::None)
}

/// `property.__delete__(self, obj)` — CPython's deleter slot.
fn property_dunder_delete(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = property_self(args, "__delete__")?;
    let obj = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("__delete__() takes exactly 2 arguments"))?;
    let fdel = p.fdel();
    if matches!(fdel, Object::None) {
        return Err(property_unreachable_error(&p, &obj, "deleter"));
    }
    reentrant_call(&fdel, &[obj])?;
    Ok(Object::None)
}

fn b_getattr(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("getattr() requires at least 2 arguments"));
    }
    let name = match crate::attr_name_of(&args[1]) {
        Some(n) => n,
        // CPython 3.12+ names the offending type (test_builtin
        // test_getattr/test_setattr/test_hasattr/test_delattr).
        None => {
            return Err(type_error(format!(
                "attribute name must be string, not '{}'",
                args[1].type_name()
            )))
        }
    };
    let default = args.get(2).cloned();
    match attr_get(&args[0], &name) {
        Some(v) => Ok(v),
        None => match default {
            Some(d) => Ok(d),
            None => Err(crate::error::attribute_error(format!(
                "'{}' object has no attribute '{}'",
                args[0].type_name(),
                name
            ))),
        },
    }
}

fn b_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 3 {
        return Err(type_error("setattr() takes exactly 3 arguments"));
    }
    let name = match crate::attr_name_of(&args[1]) {
        Some(n) => n,
        // CPython 3.12+ names the offending type (test_builtin
        // test_getattr/test_setattr/test_hasattr/test_delattr).
        None => {
            return Err(type_error(format!(
                "attribute name must be string, not '{}'",
                args[1].type_name()
            )))
        }
    };
    attr_set(&args[0], &name, args[2].clone())?;
    Ok(Object::None)
}

fn b_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("delattr() takes exactly 2 arguments"));
    }
    let name = match crate::attr_name_of(&args[1]) {
        Some(n) => n,
        // CPython 3.12+ names the offending type (test_builtin
        // test_getattr/test_setattr/test_hasattr/test_delattr).
        None => {
            return Err(type_error(format!(
                "attribute name must be string, not '{}'",
                args[1].type_name()
            )))
        }
    };
    attr_delete(&args[0], &name)?;
    Ok(Object::None)
}

fn b_hasattr(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("hasattr() takes exactly 2 arguments"));
    }
    let name = match crate::attr_name_of(&args[1]) {
        Some(n) => n,
        // CPython 3.12+ names the offending type (test_builtin
        // test_getattr/test_setattr/test_hasattr/test_delattr).
        None => {
            return Err(type_error(format!(
                "attribute name must be string, not '{}'",
                args[1].type_name()
            )))
        }
    };
    Ok(Object::Bool(attr_get(&args[0], &name).is_some()))
}

fn b_vars(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        // A `__slots__`-only instance (no implicit `__dict__` anywhere in its
        // MRO) has no mapping for `vars()` to return — CPython raises here
        // rather than handing back an empty dict (`test_statistics`
        // `NormalDist.test_slots`).
        Some(Object::Instance(inst)) if inst.cls().forbids_dict => {
            Err(type_error("vars() argument must have __dict__ attribute"))
        }
        Some(Object::Instance(inst)) => Ok(Object::Dict(inst.dict.clone())),
        Some(Object::Module(m)) => Ok(Object::Dict(m.dict.clone())),
        Some(Object::Type(t)) => Ok(Object::Dict(t.dict.clone())),
        Some(other) => Err(type_error(format!(
            "vars() argument must have __dict__, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error("vars() with no argument requires a frame")),
    }
}

/// Placeholder body for the `__import__` builtin. The VM rewrites
/// calls to this entry point before they reach this code; the
/// closure is only here so the registry has a well-typed value to
/// hand back when callers ask for `builtins.__import__`.
fn b_import_placeholder(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::runtime_error(
        "__import__ requires the VM context; call from within a running interpreter",
    ))
}

/// Placeholder body for `compile`/`exec`/`eval`. The VM intercepts
/// these before they reach this function (they need to compile
/// Python source and execute it against the calling frame's
/// globals, both of which require access to the interpreter).
fn b_vm_intrinsic(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::runtime_error(
        "this builtin must be invoked through the WeavePy interpreter",
    ))
}

/// `callable(v)` — shared with the VM's `iter(v, sentinel)` validation.
pub(crate) fn object_is_callable(v: &Object) -> bool {
    let intrinsic = matches!(
        v,
        Object::Function(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Type(_)
            // Since Python 3.10 (bpo-43682) `staticmethod` objects are
            // themselves callable, forwarding to the wrapped function.
            | Object::StaticMethod(_)
    );
    if intrinsic {
        return true;
    }
    // Instances are callable when their class exposes `__call__`.
    if let Object::Instance(inst) = v {
        return inst.cls().lookup("__call__").is_some();
    }
    false
}

fn b_callable(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "callable")?;
    Ok(Object::Bool(object_is_callable(v)))
}

fn b_object(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = crate::builtin_types::builtin_types().object_.clone();
    let inst = crate::types::PyInstance::new(cls);
    Ok(Object::Instance(Rc::new(inst)))
}

fn b_globals(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Without access to the active frame, return an empty dict; the
    // VM patches this up via its own intrinsic when calling.
    Ok(Object::new_dict())
}

fn b_locals(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_dict())
}

/// Generic attribute reader that mirrors a subset of `LoadAttr` for
/// use from the `getattr`/`hasattr` builtins.
/// Apply the small subset of the descriptor protocol that
/// [`attr_get`] (the `getattr` / `hasattr` fast path) is allowed to
/// run without the VM at hand. We bind ordinary Python functions to
/// their receiver so `getattr(inst, "m")()` matches the behaviour of
/// `inst.m()`; classmethods and staticmethods are unwrapped to the
/// same forms the VM produces for `LoadAttr`. Other descriptors —
/// `property`, `__get__` on user types — are left untouched and the
/// caller will see the raw object; full semantics require the VM.
fn bind_descriptor(value: &Object, receiver: &Object) -> Object {
    match value {
        Object::Function(_) => Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(
            receiver.clone(),
            value.clone(),
        ))),
        Object::StaticMethod(inner) => inner.func(),
        Object::ClassMethod(inner) => {
            let cls = match receiver {
                Object::Instance(inst) => Object::Type(inst.cls()),
                Object::Type(_) => receiver.clone(),
                _ => receiver.clone(),
            };
            Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(cls, inner.func())))
        }
        _ => value.clone(),
    }
}

fn attr_get(obj: &Object, name: &str) -> Option<Object> {
    match obj {
        Object::Instance(inst) => {
            if let Some(v) = inst
                .dict
                .borrow()
                .get(&crate::object::DictKey(Object::from_str(name)))
                .cloned()
            {
                return Some(v);
            }
            if let Some(v) = inst.cls().lookup(name) {
                // Bind functions to the receiver so `getattr(inst, 'm')()`
                // works the same as `inst.m()`. Other descriptors are
                // left to the VM's full `descriptor_get` path.
                return Some(bind_descriptor(&v, obj));
            }
            match name {
                "__dict__" => Some(Object::Dict(inst.dict.clone())),
                "__class__" => Some(Object::Type(inst.cls())),
                _ => None,
            }
        }
        Object::Module(m) => m
            .dict
            .borrow()
            .get(&crate::object::DictKey(Object::from_str(name)))
            .cloned(),
        Object::Type(t) => {
            if let Some(v) = t.lookup(name) {
                // Accessing an attribute *on the class* runs the descriptor
                // protocol with no instance (`Vm::descriptor_get(attr, None,
                // owner=class)`): classmethods bind to the class, staticmethods
                // unwrap, and plain functions/properties/data stay as-is
                // (`C.method` is a plain function in Python 3). Without this
                // binding `getattr(Cls, "a_classmethod")` returns the raw
                // `classmethod` descriptor, which is not callable.
                return Some(match v {
                    Object::ClassMethod(inner) => Object::BoundMethod(Rc::new(
                        crate::object::BoundMethod::new(Object::Type(t.clone()), inner.func()),
                    )),
                    Object::StaticMethod(inner) => inner.func(),
                    other => other,
                });
            }
            // Mirror the synthetic dunders served by `Vm::load_attr_type`.
            // We can't reach the VM from here, but these are pure data
            // reads off the TypeObject and safe to inline.
            match name {
                "__name__" | "__qualname__" => Some(Object::from_str(&t.name)),
                "__bases__" => Some(Object::new_tuple(
                    t.bases
                        .borrow()
                        .iter()
                        .map(|b| Object::Type(b.clone()))
                        .collect(),
                )),
                "__mro__" => Some(Object::new_tuple(
                    t.mro
                        .borrow()
                        .iter()
                        .map(|b| Object::Type(b.clone()))
                        .collect(),
                )),
                "__dict__" => Some(Object::Dict(t.dict.clone())),
                _ => None,
            }
        }
        Object::Function(f) => {
            if crate::object::is_function_slot(name) {
                if let Some(v) = f.slot(name) {
                    return Some(v);
                }
            } else if let Some(v) = f
                .attrs()
                .borrow()
                .get(&crate::object::DictKey(Object::from_str(name)))
                .cloned()
            {
                return Some(v);
            }
            // Synthetic dunders. Mirror `Vm::load_attr`'s function
            // branch so introspection routes (`hasattr`, `getattr`,
            // `inspect.iscoroutinefunction`) agree with direct
            // attribute access.
            match name {
                "__name__" | "__qualname__" => Some(Object::from_str(&f.name)),
                "__doc__" => Some(code_docstring(&f.code()).unwrap_or(Object::None)),
                "__dict__" => Some(Object::Dict(f.attrs())),
                "__code__" => Some(Object::Code(f.code())),
                "__globals__" => Some(Object::Dict(f.globals.clone())),
                "__defaults__" => {
                    if f.defaults.is_empty() {
                        Some(Object::None)
                    } else {
                        Some(Object::new_tuple(f.defaults.clone()))
                    }
                }
                "__kwdefaults__" => {
                    if f.kw_defaults.is_empty() {
                        Some(Object::None)
                    } else {
                        let mut d = crate::object::DictData::default();
                        for (k, v) in &f.kw_defaults {
                            d.insert(crate::object::DictKey(Object::from_str(k)), v.clone());
                        }
                        Some(Object::Dict(Rc::new(RefCell::new(d))))
                    }
                }
                "__closure__" => {
                    if f.closure.is_empty() {
                        Some(Object::None)
                    } else {
                        Some(Object::new_tuple(f.closure.clone()))
                    }
                }
                _ => None,
            }
        }
        Object::Code(c) => code_synthetic_attr(c, name),
        Object::Builtin(b) => match name {
            "__name__" | "__qualname__" => Some(Object::from_static(b.name)),
            // Mirror the LOAD_ATTR fast path (`Vm::load_attr`): a native
            // module's functions report their module (`os.getpid.__module__
            // == "os"`), falling back to `"builtins"` for un-attributed
            // builtins. Keeps `getattr(fn, "__module__")` / `hasattr` /
            // `pickle` agreeing with direct attribute access.
            "__module__" => Some(Object::from_static(
                crate::descr_registry::module_of(obj).unwrap_or("builtins"),
            )),
            "__doc__" => Some(Object::None),
            "__self__" => Some(Object::None),
            "__objclass__" => crate::builtin_types::builtin_fn_objclass(b).map(Object::Type),
            _ => None,
        },
        Object::BoundMethod(bm) => match name {
            "__func__" => Some(bm.function.clone()),
            "__self__" => Some(bm.receiver.clone()),
            // Defining class of a bound built-in method: the MRO entry
            // of the receiver's class that provides this method name.
            "__objclass__" => {
                if let Object::Builtin(_) = &bm.function {
                    let cls = class_of(&bm.receiver);
                    let mro: Vec<Rc<crate::types::TypeObject>> = cls.mro.borrow().clone();
                    let method_name = match &bm.function {
                        Object::Builtin(b) => b.name,
                        _ => return None,
                    };
                    let key = DictKey(Object::from_static(method_name));
                    for t in mro.iter() {
                        if t.dict.borrow().contains_key(&key) {
                            return Some(Object::Type(t.clone()));
                        }
                    }
                    return Some(Object::Type(cls));
                }
                None
            }
            "__name__" => match &bm.function {
                Object::Function(f) => Some(Object::from_str(f.name.clone())),
                Object::Builtin(b) => Some(Object::from_static(b.name)),
                _ => None,
            },
            "__code__" => match &bm.function {
                Object::Function(f) => Some(Object::Code(f.code())),
                _ => None,
            },
            "__doc__" => Some(Object::None),
            _ => None,
        },
        _ => {
            // Fall through to the method-dispatch table for built-in
            // containers (list, tuple, dict, set, str, bytes, ...).
            // CPython exposes these methods as bound attributes; `dir`
            // / `hasattr` / `getattr` should agree with attribute
            // access via the dot operator.
            if let Some(builtin) = lookup_method(obj, name) {
                return Some(Object::BoundMethod(Rc::new(
                    crate::object::BoundMethod::new(obj.clone(), builtin),
                )));
            }
            None
        }
    }
}

/// Synthetic attribute access on a [`Object::Code`]. Matches CPython's
/// `code` object surface for the fields user code commonly inspects
/// (`co_flags`, `co_name`, `co_argcount`, etc.). Returning `None` falls
/// back to the generic `AttributeError`.
pub(crate) fn code_synthetic_attr(
    c: &Rc<weavepy_compiler::CodeObject>,
    name: &str,
) -> Option<Object> {
    match name {
        "co_name" | "__name__" => Some(Object::from_str(&c.name)),
        "co_qualname" | "__qualname__" => Some(Object::from_str(if c.qualname.is_empty() {
            &c.name
        } else {
            &c.qualname
        })),
        "co_filename" => Some(Object::from_str(&c.filename)),
        "co_argcount" => Some(Object::Int(i64::from(c.arg_count))),
        "co_posonlyargcount" => Some(Object::Int(i64::from(c.posonly_count))),
        "co_kwonlyargcount" => Some(Object::Int(i64::from(c.kwonly_count))),
        "co_nlocals" => Some(Object::Int(c.varnames.len() as i64)),
        "co_stacksize" => Some(Object::Int(i64::from(
            c.wire
                .as_ref()
                .and_then(|w| w.stacksize)
                .unwrap_or_else(|| c.to_cpython().stacksize),
        ))),
        "co_flags" => Some(Object::Int(i64::from(
            c.wire
                .as_ref()
                .and_then(|w| w.flags)
                .unwrap_or_else(|| code_flags(c)),
        ))),
        "co_varnames" => Some(Object::new_tuple(
            c.varnames.iter().map(Object::from_str).collect(),
        )),
        "co_cellvars" => Some(Object::new_tuple(
            c.cellvars.iter().map(Object::from_str).collect(),
        )),
        "co_freevars" => Some(Object::new_tuple(
            c.freevars.iter().map(Object::from_str).collect(),
        )),
        "co_names" => Some(Object::new_tuple(
            c.names.iter().map(Object::from_str).collect(),
        )),
        // First *tracked* line — synthetic preamble instructions carry
        // line 0, but CPython's co_firstlineno is 1-based (a module
        // compiled from one line reports 1, not 0). A *module* code object
        // always reports 1 regardless of leading blank lines/comments
        // (test_opcodes `test_setup_annotations_line`).
        "co_firstlineno" => Some(Object::Int(if c.name == "<module>" {
            1
        } else {
            i64::from(c.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1))
        })),
        "co_consts" => Some(Object::new_tuple(
            c.constants
                .iter()
                .cloned()
                .map(crate::constant_to_object_public)
                .collect(),
        )),
        // CPython-3.13 wire view (RFC 0033). Computed on demand; a raw
        // override pinned by `CodeType(...)`/`replace()` (RFC 0060) wins
        // so constructor/replace round-trips are byte-exact.
        "co_code" => Some(Object::Bytes(
            match c.wire.as_ref().and_then(|w| w.co_code.as_deref()) {
                Some(b) => Rc::from(b.to_vec()),
                None => Rc::from(c.to_cpython().co_code.clone()),
            },
        )),
        "co_linetable" => Some(Object::Bytes(
            match c.wire.as_ref().and_then(|w| w.co_linetable.as_deref()) {
                Some(b) => Rc::from(b.to_vec()),
                None => Rc::from(c.to_cpython().co_linetable.clone()),
            },
        )),
        "co_exceptiontable" => Some(Object::Bytes(
            match c.wire.as_ref().and_then(|w| w.co_exceptiontable.as_deref()) {
                Some(b) => Rc::from(b.to_vec()),
                None => Rc::from(c.to_cpython().co_exceptiontable.clone()),
            },
        )),
        "co_localsplusnames" => Some(Object::new_tuple(
            c.to_cpython()
                .localsplusnames
                .iter()
                .map(Object::from_str)
                .collect(),
        )),
        "co_localspluskinds" => Some(Object::Bytes(Rc::from(
            c.to_cpython().localspluskinds.clone(),
        ))),
        "co_lines" => Some(code_method(c, "co_lines", code_co_lines)),
        "co_positions" => Some(code_method(c, "co_positions", code_co_positions)),
        // Deprecated pre-PEP-626 line table, derived on demand from the
        // position records exactly like CPython's `decode_linetable`
        // (`dis.findlinestarts` fallbacks and legacy tools read it).
        "co_lnotab" => Some(Object::Bytes(Rc::from(code_lnotab_bytes(c)))),
        "_varname_from_oparg" => Some(code_method(
            c,
            "_varname_from_oparg",
            code_varname_from_oparg,
        )),
        // `__replace__` is the copy.replace() protocol hook (3.13).
        "replace" | "__replace__" => Some(code_method_kw(c, "replace", code_replace)),
        _ => None,
    }
}

/// Like [`code_method`] but for a keyword-argument-accepting method
/// (`code.replace(**kwargs)`). Calling it with no kwargs returns an
/// identical copy, matching CPython.
fn code_method_kw(
    c: &Rc<weavepy_compiler::CodeObject>,
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(
        Object::Code(c.clone()),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    )))
}

/// `code.replace(**kwargs)` — return a copy of the code object with
/// the named `co_*` fields overridden (PEP 626 / `CodeType.replace`).
///
/// WeavePy stores the source-level fields directly, so those are
/// honoured exactly. Fields CPython derives from the instruction
/// stream (`co_code`, `co_linetable`, `co_stacksize`, `co_flags`, …)
/// are accepted for drop-in compatibility but carried through from the
/// original; an unknown keyword raises `TypeError`, as in CPython.
/// Decode a CPython compact location table (PEP 626 / `co_linetable`)
/// into per-unit lines; `None` marks the NO_LOCATION entries (`f_lineno`
/// shows them as None).
fn decode_compact_linetable(table: &[u8], firstlineno: u32) -> Vec<Option<u32>> {
    fn varint(table: &[u8], pos: &mut usize) -> i32 {
        let mut val: i32 = 0;
        let mut shift = 0;
        while *pos < table.len() {
            let b = table[*pos];
            *pos += 1;
            val |= i32::from(b & 0x3F) << shift;
            if b & 0x40 == 0 {
                break;
            }
            shift += 6;
        }
        val
    }
    fn svarint(table: &[u8], pos: &mut usize) -> i32 {
        let v = varint(table, pos);
        if v & 1 != 0 {
            -(v >> 1)
        } else {
            v >> 1
        }
    }
    let mut out: Vec<Option<u32>> = Vec::new();
    let mut pos = 0usize;
    let mut line = firstlineno as i32;
    while pos < table.len() {
        let first = table[pos];
        pos += 1;
        if first & 0x80 == 0 {
            break;
        }
        let code = (first >> 3) & 0x0F;
        let length = ((first & 0x07) as usize) + 1;
        let entry_line = match code {
            15 => None,
            13 => {
                line += svarint(table, &mut pos);
                Some(line)
            }
            14 => {
                line += svarint(table, &mut pos);
                let _ = varint(table, &mut pos);
                let _ = varint(table, &mut pos);
                let _ = varint(table, &mut pos);
                Some(line)
            }
            10..=12 => {
                line += i32::from(code) - 10;
                let _ = varint(table, &mut pos);
                let _ = varint(table, &mut pos);
                Some(line)
            }
            _ => {
                pos += 1;
                Some(line)
            }
        };
        for _ in 0..length {
            out.push(entry_line.map(|l| l.max(0) as u32));
        }
    }
    out
}

/// The unbound `code.__replace__` callable installed in the `code`
/// type's dict — `copy.replace(code, **changes)` resolves it on the
/// class and calls it with the receiver first (RFC 0060).
pub(crate) fn code_dunder_replace_object() -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name: "__replace__",
        binds_instance: false,
        call: Box::new(|args| code_replace(args, &[])),
        call_kw: Some(Box::new(code_replace)),
    }))
}

fn code_replace(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let c = code_self(args)?;
    let mut nc: weavepy_compiler::CodeObject = (*c).clone();

    fn want_str(o: &Object, field: &str) -> Result<String, RuntimeError> {
        match o {
            Object::Str(s) => Ok(s.to_string()),
            _ => Err(type_error(format!("code.replace(): {field} must be str"))),
        }
    }
    fn want_u32(o: &Object, field: &str) -> Result<u32, RuntimeError> {
        match o {
            Object::Int(i) if *i >= 0 => Ok(*i as u32),
            Object::Int(_) => Err(type_error(format!(
                "code.replace(): {field} must be non-negative"
            ))),
            _ => Err(type_error(format!("code.replace(): {field} must be int"))),
        }
    }
    fn want_str_seq(o: &Object, field: &str) -> Result<Vec<String>, RuntimeError> {
        let items: Vec<Object> = match o {
            Object::Tuple(t) => t.iter().cloned().collect(),
            Object::List(l) => l.borrow().iter().cloned().collect(),
            _ => {
                return Err(type_error(format!(
                    "code.replace(): {field} must be a tuple of str"
                )))
            }
        };
        items.iter().map(|it| want_str(it, field)).collect()
    }

    let mut requested_nlocals: Option<u32> = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "co_name" => nc.name = want_str(v, "co_name")?,
            "co_qualname" => nc.qualname = want_str(v, "co_qualname")?,
            "co_filename" => nc.filename = want_str(v, "co_filename")?,
            "co_argcount" => nc.arg_count = want_u32(v, "co_argcount")?,
            "co_posonlyargcount" => nc.posonly_count = want_u32(v, "co_posonlyargcount")?,
            "co_kwonlyargcount" => nc.kwonly_count = want_u32(v, "co_kwonlyargcount")?,
            "co_nlocals" => requested_nlocals = Some(want_u32(v, "co_nlocals")?),
            "co_varnames" => nc.varnames = want_str_seq(v, "co_varnames")?,
            "co_names" => nc.names = want_str_seq(v, "co_names")?,
            "co_freevars" => nc.freevars = want_str_seq(v, "co_freevars")?,
            "co_cellvars" => nc.cellvars = want_str_seq(v, "co_cellvars")?,
            "co_stacksize" => {
                nc.wire.get_or_insert_with(Default::default).stacksize =
                    Some(want_u32(v, "co_stacksize")?);
            }
            "co_flags" => {
                let flags = want_u32(v, "co_flags")?;
                nc.wire.get_or_insert_with(Default::default).flags = Some(flags);
                // The flag bits *are* the semantics in CPython: decode the
                // execution-relevant ones into the native fields so e.g.
                // `types.coroutine`'s `co.replace(co_flags=co.co_flags |
                // CO_ITERABLE_COROUTINE)` produces generators `await`
                // accepts (Lib/types.py runs verbatim).
                nc.has_varargs = flags & 0x0004 != 0;
                nc.has_varkeywords = flags & 0x0008 != 0;
                nc.is_generator = flags & 0x0020 != 0;
                nc.is_coroutine = flags & 0x0080 != 0;
                nc.is_iterable_coroutine = flags & 0x0100 != 0;
                nc.is_async_generator = flags & 0x0200 != 0;
            }
            "co_consts" => {
                let items: Vec<Object> = match v {
                    Object::Tuple(t) => t.iter().cloned().collect(),
                    Object::List(l) => l.borrow().iter().cloned().collect(),
                    _ => {
                        return Err(type_error(
                            "code.replace(): co_consts must be a tuple".to_owned(),
                        ))
                    }
                };
                nc.constants = items.iter().map(crate::object_to_constant_public).collect();
            }
            "co_code" => {
                let bytes: Vec<u8> = match v {
                    Object::Bytes(b) => b.to_vec(),
                    _ => {
                        return Err(type_error(
                            "code.replace(): co_code must be bytes".to_owned(),
                        ))
                    }
                };
                install_wire_code(&mut nc, bytes);
            }
            "co_exceptiontable" => {
                let bytes: Vec<u8> = match v {
                    Object::Bytes(b) => b.to_vec(),
                    _ => {
                        return Err(type_error(
                            "code.replace(): co_exceptiontable must be bytes".to_owned(),
                        ))
                    }
                };
                nc.wire
                    .get_or_insert_with(Default::default)
                    .co_exceptiontable = Some(bytes);
            }
            "co_firstlineno" => {
                // Shift the absolute per-instruction line table so the
                // first line reports the requested value while keeping
                // the relative line structure intact.
                let target = want_u32(v, "co_firstlineno")?;
                if let Some(&first) = nc.linetable.first() {
                    let delta = i64::from(target) - i64::from(first);
                    for l in &mut nc.linetable {
                        *l = (i64::from(*l) + delta).max(0) as u32;
                    }
                }
            }
            "co_linetable" => {
                // Re-derive per-instruction lines from a CPython compact
                // location table (PEP 626). Entries with the NO_LOCATION
                // code map to the 0 sentinel, which `f_lineno` reports
                // as None (test_missing_lineno_shows_as_none).
                let bytes: Vec<u8> = match v {
                    Object::Bytes(b) => b.to_vec(),
                    _ => {
                        return Err(type_error(
                            "code.replace(): co_linetable must be bytes".to_owned(),
                        ))
                    }
                };
                let firstlineno = nc.linetable.first().copied().unwrap_or(1);
                let unit_lines = decode_compact_linetable(&bytes, firstlineno);
                for (i, slot) in nc.linetable.iter_mut().enumerate() {
                    *slot = unit_lines.get(i).copied().flatten().unwrap_or(0);
                }
                // Pin the raw table so `co_linetable` round-trips exactly
                // and `co_lines()`/`co_positions()` report *its* entries
                // (empty table ⇒ no location info, test_empty_linetable).
                nc.wire.get_or_insert_with(Default::default).co_linetable = Some(bytes);
            }
            // Deprecated derived field: accepted, carried through.
            "co_lnotab" => {}
            other => {
                return Err(type_error(format!(
                    "replace() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // CPython validates that an explicit co_nlocals agrees with the
    // (possibly replaced) co_varnames (test_nlocals_mismatch).
    if let Some(n) = requested_nlocals {
        if n as usize != nc.varnames.len() {
            return Err(value_error(format!(
                "code: co_nlocals != len(co_varnames) ({} != {})",
                n,
                nc.varnames.len()
            )));
        }
    }
    Ok(Object::Code(Rc::new(nc)))
}

/// Build a metadata-only VM code object from a **C-minted** (foreign)
/// `PyCodeObject`'s fields — RFC 0066 WS3. Cython creates one real code
/// object per `def` during module init and stores it as the cyfunction's
/// `__code__`; when that value crosses into the VM it must be a genuine
/// `types.CodeType` instance, because `inspect`'s function-like probe is
/// `isinstance(f.__code__, types.CodeType)` — an opaque foreign proxy
/// fails it and `inspect.signature` falls through to the
/// `__text_signature__` path and raises ("no signature found for
/// builtin", scipy's `_transition_to_rng` decorator over
/// `Rotation.random` at `scipy.spatial.transform._rotation_cy` init).
/// The object carries the introspection surface (names, counts, flags,
/// varnames) and is deliberately not executable.
#[allow(clippy::too_many_arguments)]
pub fn foreign_code_object(
    name: String,
    qualname: String,
    filename: String,
    firstlineno: u32,
    arg_count: u32,
    posonly_count: u32,
    kwonly_count: u32,
    flags: u32,
    varnames: Vec<String>,
) -> Object {
    const CO_VARARGS: u32 = 0x0004;
    const CO_VARKEYWORDS: u32 = 0x0008;
    const CO_GENERATOR: u32 = 0x0020;
    const CO_COROUTINE: u32 = 0x0080;
    const CO_ITERABLE_COROUTINE: u32 = 0x0100;
    const CO_ASYNC_GENERATOR: u32 = 0x0200;
    let mut nc = weavepy_compiler::CodeObject {
        name,
        qualname,
        filename,
        varnames,
        arg_count,
        posonly_count,
        kwonly_count,
        has_varargs: flags & CO_VARARGS != 0,
        has_varkeywords: flags & CO_VARKEYWORDS != 0,
        is_generator: flags & CO_GENERATOR != 0,
        is_coroutine: flags & CO_COROUTINE != 0,
        is_iterable_coroutine: flags & CO_ITERABLE_COROUTINE != 0,
        is_async_generator: flags & CO_ASYNC_GENERATOR != 0,
        ..Default::default()
    };
    nc.linetable = vec![firstlineno.max(1)];
    let w = nc.wire.get_or_insert_with(Default::default);
    w.co_code = Some(Vec::new());
    w.exec_error = Some("cannot execute foreign bytecode".to_owned());
    Object::Code(Rc::new(nc))
}

/// Pin raw CPython `co_code` bytes on `nc` (RFC 0060 — `CodeType(...)` /
/// `replace(co_code=…)`). When the stream decodes back into WeavePy
/// instructions the code object stays executable; otherwise executing it
/// raises `SystemError` (CPython: "unknown opcode N",
/// test_code.test_invalid_bytecode).
fn install_wire_code(nc: &mut weavepy_compiler::CodeObject, bytes: Vec<u8>) {
    let slots = weavepy_compiler::cpython_code::SlotMap::from_code_vars(
        &nc.varnames,
        &nc.cellvars,
        &nc.freevars,
    );
    match weavepy_compiler::cpython_code::decode(&bytes, &slots, &nc.constants) {
        Some(instructions) => {
            // Keep per-instruction side tables in sync with the new
            // instruction count; line info defaults to the first line.
            let first = nc.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1);
            nc.linetable = vec![first; instructions.len()];
            nc.coltable = Vec::new();
            nc.caches = weavepy_compiler::CacheTable::with_len(instructions.len());
            nc.instructions = instructions;
            nc.exception_table = Vec::new();
            let w = nc.wire.get_or_insert_with(Default::default);
            w.co_code = Some(bytes);
            w.exec_error = None;
        }
        None => {
            let msg = match weavepy_compiler::cpython_code::first_unknown_opcode(&bytes) {
                Some(op) => format!("unknown opcode {op}"),
                None => "cannot execute foreign bytecode".to_owned(),
            };
            let w = nc.wire.get_or_insert_with(Default::default);
            w.co_code = Some(bytes);
            w.exec_error = Some(msg);
        }
    }
}

/// `types.CodeType(argcount, posonlyargcount, kwonlyargcount, nlocals,
/// stacksize, flags, codestring, constants, names, varnames, filename,
/// name, qualname, firstlineno, linetable, exceptiontable[, freevars[,
/// cellvars]])` — CPython's `code_new` (RFC 0060). The raw wire fields
/// are pinned on the result so the attribute surface round-trips
/// byte-exactly; when the bytecode decodes into WeavePy instructions the
/// object is also executable.
pub(crate) fn code_type_call(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    use weavepy_compiler::cpython_code::{CO_FAST_CELL, CO_FAST_FREE, CO_FAST_LOCAL};
    if !kwargs.is_empty() {
        return Err(type_error("code() takes no keyword arguments"));
    }
    if args.len() < 16 || args.len() > 18 {
        return Err(type_error(format!(
            "code expected at most 18 arguments, got {}",
            args.len()
        )));
    }
    fn want_int(o: &Object, field: &str) -> Result<u32, RuntimeError> {
        match o {
            Object::Int(i) if *i >= 0 => Ok(*i as u32),
            Object::Int(_) => Err(value_error(format!("code: {field} must be non-negative"))),
            _ => Err(type_error(format!(
                "code: {field} must be int, not {}",
                o.type_name()
            ))),
        }
    }
    fn want_bytes(o: &Object, field: &str) -> Result<Vec<u8>, RuntimeError> {
        match o {
            Object::Bytes(b) => Ok(b.to_vec()),
            _ => Err(type_error(format!(
                "code: {field} must be bytes, not {}",
                o.type_name()
            ))),
        }
    }
    fn want_str(o: &Object, field: &str) -> Result<String, RuntimeError> {
        match o {
            Object::Str(s) => Ok(s.to_string()),
            _ => Err(type_error(format!(
                "code: {field} must be str, not {}",
                o.type_name()
            ))),
        }
    }
    fn want_str_tuple(o: &Object, field: &str) -> Result<Vec<String>, RuntimeError> {
        match o {
            Object::Tuple(t) => t.iter().map(|it| want_str(it, field)).collect(),
            _ => Err(type_error(format!(
                "code: {field} must be tuple, not {}",
                o.type_name()
            ))),
        }
    }
    let arg_count = want_int(&args[0], "argcount")?;
    let posonly_count = want_int(&args[1], "posonlyargcount")?;
    let kwonly_count = want_int(&args[2], "kwonlyargcount")?;
    let nlocals = want_int(&args[3], "nlocals")?;
    let stacksize = want_int(&args[4], "stacksize")?;
    let flags = want_int(&args[5], "flags")?;
    let codestring = want_bytes(&args[6], "codestring")?;
    let constants = match &args[7] {
        Object::Tuple(t) => t
            .iter()
            .map(crate::object_to_constant_public)
            .collect::<Vec<_>>(),
        other => {
            return Err(type_error(format!(
                "code: constants must be tuple, not {}",
                other.type_name()
            )))
        }
    };
    let names = want_str_tuple(&args[8], "names")?;
    let varnames = want_str_tuple(&args[9], "varnames")?;
    let filename = want_str(&args[10], "filename")?;
    let name = want_str(&args[11], "name")?;
    let qualname = want_str(&args[12], "qualname")?;
    let firstlineno = want_int(&args[13], "firstlineno")?;
    let linetable = want_bytes(&args[14], "linetable")?;
    let exceptiontable = want_bytes(&args[15], "exceptiontable")?;
    let freevars = match args.get(16) {
        Some(o) => want_str_tuple(o, "freevars")?,
        None => Vec::new(),
    };
    let cellvars = match args.get(17) {
        Some(o) => want_str_tuple(o, "cellvars")?,
        None => Vec::new(),
    };
    if nlocals as usize != varnames.len() {
        return Err(value_error(format!(
            "code: co_nlocals != len(co_varnames) ({} != {})",
            nlocals,
            varnames.len()
        )));
    }
    // Locals-plus layout mirrors `encode`: plain locals, then cells,
    // then frees — with a cell aliasing a local (an escaping parameter)
    // sharing the local's slot (CO_FAST_LOCAL|CO_FAST_CELL), exactly as
    // CPython's `_PyCode_New` merges them.
    let mut localsplusnames: Vec<String> = Vec::new();
    let mut localspluskinds: Vec<u8> = Vec::new();
    for v in &varnames {
        let mut kind = CO_FAST_LOCAL;
        if cellvars.iter().any(|c| c == v) {
            kind |= CO_FAST_CELL;
        }
        localsplusnames.push(v.clone());
        localspluskinds.push(kind);
    }
    for v in &cellvars {
        if varnames.iter().any(|n| n == v) {
            continue;
        }
        localsplusnames.push(v.clone());
        localspluskinds.push(CO_FAST_CELL);
    }
    for v in &freevars {
        localsplusnames.push(v.clone());
        localspluskinds.push(CO_FAST_FREE);
    }
    const CO_VARARGS: u32 = 0x0004;
    const CO_VARKEYWORDS: u32 = 0x0008;
    const CO_GENERATOR: u32 = 0x0020;
    const CO_COROUTINE: u32 = 0x0080;
    const CO_ITERABLE_COROUTINE: u32 = 0x0100;
    const CO_ASYNC_GENERATOR: u32 = 0x0200;
    let mut nc = weavepy_compiler::CodeObject {
        name,
        qualname,
        filename,
        constants,
        names,
        varnames,
        freevars,
        cellvars,
        arg_count,
        posonly_count,
        kwonly_count,
        has_varargs: flags & CO_VARARGS != 0,
        has_varkeywords: flags & CO_VARKEYWORDS != 0,
        is_generator: flags & CO_GENERATOR != 0,
        is_coroutine: flags & CO_COROUTINE != 0,
        is_iterable_coroutine: flags & CO_ITERABLE_COROUTINE != 0,
        is_async_generator: flags & CO_ASYNC_GENERATOR != 0,
        future_flags: flags & weavepy_compiler::flags::PYCF_MASK,
        ..Default::default()
    };
    match weavepy_compiler::cpython_code::decode_full(
        &codestring,
        &linetable,
        &exceptiontable,
        &localsplusnames,
        &localspluskinds,
        firstlineno,
        &nc.constants,
    ) {
        Some(decoded) => {
            nc.caches = weavepy_compiler::CacheTable::with_len(decoded.instructions.len());
            nc.instructions = decoded.instructions;
            nc.linetable = decoded.linetable;
            nc.coltable = decoded.coltable;
            nc.exception_table = decoded.exception_table;
            nc.no_interrupt_jumps = decoded.no_interrupt_jumps;
        }
        None => {
            let msg = match weavepy_compiler::cpython_code::first_unknown_opcode(&codestring) {
                Some(op) => format!("unknown opcode {op}"),
                None => "cannot execute foreign bytecode".to_owned(),
            };
            nc.linetable = vec![firstlineno.max(1)];
            nc.wire.get_or_insert_with(Default::default).exec_error = Some(msg);
        }
    }
    {
        let w = nc.wire.get_or_insert_with(Default::default);
        w.co_code = Some(codestring);
        w.co_linetable = Some(linetable);
        w.co_exceptiontable = Some(exceptiontable);
        w.stacksize = Some(stacksize);
        w.flags = Some(flags);
    }
    Ok(Object::Code(Rc::new(nc)))
}

/// Wrap a native code-object method as a bound method whose receiver is
/// the code object (delivered to `body` as `args[0]`).
fn code_method(
    c: &Rc<weavepy_compiler::CodeObject>,
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(
        Object::Code(c.clone()),
        Object::Builtin(Rc::new(method(name, body))),
    )))
}

/// Extract the receiver code object from a bound-method call's `args[0]`.
fn code_self(args: &[Object]) -> Result<Rc<weavepy_compiler::CodeObject>, RuntimeError> {
    match args.first() {
        Some(Object::Code(c)) => Ok(c.clone()),
        _ => Err(type_error(
            "descriptor of 'code' object needs a code receiver".to_owned(),
        )),
    }
}

/// `code.co_positions()` — one `(lineno, end_lineno, col, end_col)` tuple
/// per code unit (PEP 657). Columns are `None` until column plumbing
/// lands (RFC 0033 follow-up).
fn code_co_positions(args: &[Object]) -> Result<Object, RuntimeError> {
    let c = code_self(args)?;
    // A pinned raw linetable (RFC 0060 `replace(co_linetable=…)`) is the
    // sole source of location info: decode its entries directly (an empty
    // table yields an empty iterator, test_co_positions_empty_linetable).
    if let Some(t) = c.wire.as_ref().and_then(|w| w.co_linetable.as_deref()) {
        let first = c.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1);
        let items = decode_compact_linetable(t, first)
            .into_iter()
            .map(|line| {
                let l = line.map_or(Object::None, |v| Object::Int(i64::from(v)));
                Object::new_tuple(vec![l.clone(), l, Object::None, Object::None])
            })
            .collect();
        return list_iter(items);
    }
    let cp = c.to_cpython();
    let debug_ranges = crate::vm_singletons::debug_ranges();
    let col = |v: Option<u32>| {
        v.filter(|_| debug_ranges)
            .map_or(Object::None, |x| Object::Int(i64::from(x)))
    };
    let line = |v: i32| {
        // -1 marks NO_LOCATION; 0 is a real line (module RESUME).
        if v < 0 {
            Object::None
        } else {
            Object::Int(i64::from(v))
        }
    };
    let items = cp
        .positions
        .iter()
        .map(|p| {
            // A NO_LOCATION unit (lineno 0) reports all-None (PEP 657).
            // With debug ranges disabled only start lines survive, so
            // end_line collapses onto line (CPython stores no
            // end-position table; test_endline_and_columntable_none_…).
            let end_lineno = if debug_ranges { p.end_lineno } else { p.lineno };
            Object::new_tuple(vec![
                line(p.lineno),
                line(end_lineno),
                col(p.col),
                col(p.end_col),
            ])
        })
        .collect();
    list_iter(items)
}

/// Wrap a vector of objects as a single-use iterator, mirroring the
/// iterators CPython's `co_positions()` / `co_lines()` return.
fn list_iter(items: Vec<Object>) -> Result<Object, RuntimeError> {
    let it = Object::new_list(items).make_iter()?;
    Ok(Object::Iter(Rc::new(RefCell::new(it))))
}

/// `code.co_lnotab` — the deprecated pre-PEP-626 `(addr_delta,
/// line_delta)` byte-pair encoding, rebuilt from the per-unit position
/// records (CPython `decode_linetable` + `write_lnotab`, including the
/// unsigned-255 address chunking and the signed [-128, 127] line-delta
/// wraparound).
fn code_lnotab_bytes(c: &Rc<weavepy_compiler::CodeObject>) -> Vec<u8> {
    let cp = c.to_cpython();
    let mut out: Vec<u8> = Vec::new();
    let write_pair = |out: &mut Vec<u8>, bdelta: i32, ldelta: i32| {
        out.push(bdelta as u8);
        out.push(ldelta as i8 as u8);
    };
    let write_lnotab = |out: &mut Vec<u8>, mut bdelta: i32, mut ldelta: i32| {
        while bdelta > 255 {
            write_pair(out, 255, 0);
            bdelta -= 255;
        }
        while ldelta > 127 {
            write_pair(out, bdelta, 127);
            bdelta = 0;
            ldelta -= 127;
        }
        while ldelta < -128 {
            write_pair(out, bdelta, -128);
            bdelta = 0;
            ldelta += 128;
        }
        write_pair(out, bdelta, ldelta);
    };
    let mut code_offset: i32 = 0;
    let mut line: i32 = cp.firstlineno as i32;
    let n = cp.positions.len();
    let mut i = 0;
    while i < n {
        let range_line = cp.positions[i].lineno;
        let start = (i * 2) as i32;
        while i < n && cp.positions[i].lineno == range_line {
            i += 1;
        }
        // Location-less units (line 0 / -1) never open a new lnotab
        // entry; they inherit the previous line, as in CPython.
        if range_line > 0 && range_line != line {
            write_lnotab(&mut out, start - code_offset, range_line - line);
            code_offset = start;
            line = range_line;
        }
    }
    out
}

/// `code.co_lines()` — `(start, end, lineno)` byte ranges (PEP 626),
/// merging consecutive code units that share a line.
fn code_co_lines(args: &[Object]) -> Result<Object, RuntimeError> {
    let c = code_self(args)?;
    // Pinned raw linetable (RFC 0060): its entries are the whole story —
    // an empty table means no line info at all (test_empty_linetable).
    let pinned: Option<Vec<i32>> =
        c.wire
            .as_ref()
            .and_then(|w| w.co_linetable.as_deref())
            .map(|t| {
                let first = c.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1);
                decode_compact_linetable(t, first)
                    .into_iter()
                    .map(|l| l.map_or(-1i32, |v| v as i32))
                    .collect()
            });
    let lines: Vec<i32> = match pinned {
        Some(v) => v,
        None => c.to_cpython().positions.iter().map(|p| p.lineno).collect(),
    };
    let n = lines.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let line = lines[i];
        let start = i;
        while i < n && lines[i] == line {
            i += 1;
        }
        out.push(Object::new_tuple(vec![
            Object::Int((start * 2) as i64),
            Object::Int((i * 2) as i64),
            // PEP 626: a range with no source line yields None (-1
            // marks NO_LOCATION; 0 is real — the module RESUME).
            if line < 0 {
                Object::None
            } else {
                Object::Int(i64::from(line))
            },
        ]));
    }
    list_iter(out)
}

/// `code._varname_from_oparg(i)` — resolve a fast-local / cell / free
/// index into its name (`co_localsplusnames[i]`). `dis` uses this to
/// label `LOAD_FAST` / `LOAD_DEREF`.
fn code_varname_from_oparg(args: &[Object]) -> Result<Object, RuntimeError> {
    let c = code_self(args)?;
    let idx = match args.get(1) {
        Some(Object::Int(i)) if *i >= 0 => *i as usize,
        _ => {
            return Err(type_error(
                "_varname_from_oparg() requires a non-negative int".to_owned(),
            ))
        }
    };
    // localsplus order: plain locals, then cells *not aliasing a local*
    // (an escaping parameter's cell shares the parameter's slot), then
    // frees — the same dedup as the wire encoder's `build_localsplus`.
    c.varnames
        .iter()
        .chain(
            c.cellvars
                .iter()
                .filter(|cv| !c.varnames.iter().any(|v| v == *cv)),
        )
        .chain(c.freevars.iter())
        .nth(idx)
        .map(Object::from_str)
        .ok_or_else(|| type_error("_varname_from_oparg(): index out of range".to_owned()))
}

thread_local! {
    /// Docstring objects keyed by the constant's string-data address, so
    /// repeated `f.__doc__` reads return the *same* `str` object (CPython
    /// stores the docstring once on the function; `update_wrapper` tests
    /// `assertIs(wrapper.__doc__, wrapped.__doc__)`).
    static DOCSTRING_CACHE: std::cell::RefCell<std::collections::HashMap<usize, Object>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Return the docstring extracted from a code object, if its first
/// constant is a string literal — CPython's `__doc__` convention.
/// The compiler keeps the leading bare string expression as
/// ``constants[0]``; functions / modules / classes pick it up at
/// runtime via this helper.
pub(crate) fn code_docstring(c: &weavepy_compiler::CodeObject) -> Option<Object> {
    match c.constants.first() {
        Some(weavepy_compiler::Constant::Str(s)) => Some(DOCSTRING_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let key = s.as_ptr() as usize;
            // The address key is subject to ABA reuse: dropping a code
            // object frees its constant buffers, and a later compile can
            // land a *different* docstring at the same address (doctest
            // compiles thousands of transient `<doctest …>` snippets and
            // was nondeterministically reading sibling examples'
            // docstrings). Trust the entry only if the content still
            // matches; otherwise replace it.
            if let Some(Object::Str(cached)) = cache.get(&key) {
                if cached.as_ref() == s.as_str() {
                    return Object::Str(cached.clone());
                }
            }
            let fresh = Object::from_str(s.as_str());
            cache.insert(key, fresh.clone());
            fresh
        })),
        _ => None,
    }
}

/// Compose CPython-shaped `co_flags` for a [`weavepy_compiler::CodeObject`].
/// We carry the same flag bits CPython does for the cases the
/// introspection ecosystem checks for: vararg / kwarg presence,
/// generator / coroutine / async-generator status, and the implicit
/// `OPTIMIZED | NEWLOCALS` pair every function frame uses.
pub(crate) fn code_flags(c: &weavepy_compiler::CodeObject) -> u32 {
    const CO_OPTIMIZED: u32 = 0x0001;
    const CO_NEWLOCALS: u32 = 0x0002;
    const CO_VARARGS: u32 = 0x0004;
    const CO_VARKEYWORDS: u32 = 0x0008;
    const CO_NESTED: u32 = 0x0010;
    const CO_GENERATOR: u32 = 0x0020;
    const CO_COROUTINE: u32 = 0x0080;
    const CO_ITERABLE_COROUTINE: u32 = 0x0100;
    const CO_ASYNC_GENERATOR: u32 = 0x0200;
    // Only *function* scopes are OPTIMIZED|NEWLOCALS (fast locals +
    // fresh namespace). Module and class bodies run over a mapping and
    // report 0x0, as CPython's compiler_enter_scope sets them
    // (test_dis's code_info asserts `Flags: 0x0` on compiled source).
    let mut f = if c.is_class_body || c.name == "<module>" {
        0
    } else {
        CO_OPTIMIZED | CO_NEWLOCALS
    };
    // CO_NESTED marks code compiled inside a function scope. The
    // qualname records exactly that nesting ("outer.<locals>.inner",
    // PEP 3155), so it is the compile-time signal we retained.
    // (CPython 3.13 no longer sets CO_NOFREE — the 0x40 bit is dead.)
    if c.qualname.contains("<locals>.") {
        f |= CO_NESTED;
    }
    if c.has_varargs {
        f |= CO_VARARGS;
    }
    if c.has_varkeywords {
        f |= CO_VARKEYWORDS;
    }
    if c.is_generator {
        f |= CO_GENERATOR;
    }
    if c.is_coroutine {
        f |= CO_COROUTINE;
    }
    if c.is_iterable_coroutine {
        f |= CO_ITERABLE_COROUTINE;
    }
    if c.is_async_generator {
        f |= CO_ASYNC_GENERATOR;
    }
    // `CO_FUTURE_*` bits recorded at compile time (RFC 0052) — what
    // lets `compile(..., dont_inherit=False)` inherit the caller's
    // future statements, like CPython.
    f | c.future_flags
}

fn attr_set(obj: &Object, name: &str, value: Object) -> Result<(), RuntimeError> {
    match obj {
        Object::Instance(inst) => {
            inst.dict
                .borrow_mut()
                .insert(crate::object::DictKey(Object::from_str(name)), value);
            Ok(())
        }
        Object::Module(m) => {
            m.dict
                .borrow_mut()
                .insert(crate::object::DictKey(Object::from_str(name)), value);
            Ok(())
        }
        Object::Type(t) => {
            t.dict
                .borrow_mut()
                .insert(crate::object::DictKey(Object::from_str(name)), value);
            Ok(())
        }
        Object::Function(f) => {
            if name == "__code__" {
                let Object::Code(c) = value else {
                    return Err(type_error("__code__ must be set to a code object"));
                };
                if f.closure.len() != c.freevars.len() {
                    return Err(crate::error::value_error(format!(
                        "{}() requires a code object with {} free vars, not {}",
                        f.name,
                        f.closure.len(),
                        c.freevars.len()
                    )));
                }
                *f.code.borrow_mut() = c;
            } else if crate::object::is_function_slot(name) {
                f.set_slot(name, value);
            } else {
                f.attrs()
                    .borrow_mut()
                    .insert(crate::object::DictKey(Object::from_str(name)), value);
            }
            Ok(())
        }
        // Methods carry no `__dict__`; metadata belongs on `__func__`
        // (CPython `PyMethod_Type` — test_funcattrs).
        Object::BoundMethod(_) => Err(crate::bound_method_readonly_error(name, false)),
        _ => Err(type_error(format!(
            "'{}' object has no attribute '{}'",
            obj.type_name(),
            name
        ))),
    }
}

fn attr_delete(obj: &Object, name: &str) -> Result<(), RuntimeError> {
    match obj {
        Object::Instance(inst) => {
            inst.dict
                .borrow_mut()
                .shift_remove(&crate::object::DictKey(Object::from_str(name)));
            Ok(())
        }
        Object::Module(m) => {
            m.dict
                .borrow_mut()
                .shift_remove(&crate::object::DictKey(Object::from_str(name)));
            Ok(())
        }
        Object::Function(f) => {
            if crate::object::is_function_slot(name) {
                f.slots
                    .borrow_mut()
                    .shift_remove(&crate::object::DictKey(Object::from_str(name)));
            } else {
                f.attrs()
                    .borrow_mut()
                    .shift_remove(&crate::object::DictKey(Object::from_str(name)));
            }
            Ok(())
        }
        // Same taxonomy as assignment: methods carry no `__dict__`.
        Object::BoundMethod(_) => Err(crate::bound_method_readonly_error(name, true)),
        _ => Err(type_error(format!("cannot delete attribute '{}'", name))),
    }
}

fn b_int(args: &[Object]) -> Result<Object, RuntimeError> {
    b_int_compat(args)
}

/// `int(x)` for the subset of input shapes that don't need a VM
/// (literals, strings, numbers). Used both as the bare-bones registry
/// entry and as a helper from the VM-aware dispatch path.
pub(crate) fn b_int_compat(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Int(0));
    }
    match &args[0] {
        Object::Int(i) => Ok(Object::Int(*i)),
        Object::Long(b) => Ok(Object::Long(b.clone())),
        Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
        Object::Float(f) => {
            if !f.is_finite() {
                // CPython: `int(nan)` → ValueError, `int(±inf)` → OverflowError.
                return Err(if f.is_nan() {
                    value_error("cannot convert float NaN to integer")
                } else {
                    crate::error::overflow_error("cannot convert float infinity to integer")
                });
            }
            // Truncate toward zero, like Python.
            let truncated = f.trunc();
            #[allow(clippy::float_cmp)]
            let exact = |x: i64| {
                if (x as f64) == truncated {
                    Some(x)
                } else {
                    None
                }
            };
            if let Some(small) = i64::try_from(truncated as i128).ok().and_then(exact) {
                return Ok(Object::Int(small));
            }
            Ok(Object::int_from_bigint(
                crate::object::bigint_from_f64_trunc(truncated),
            ))
        }
        Object::Str(s) => parse_int_string(&args[0], s, &args[1..]),
        // A WTF-8 `str` (lone surrogates, stored as code points): decode to
        // text with each lone surrogate rendered as U+FFFD, which is not a
        // digit, so parsing fails with `ValueError` — and the error's `repr`
        // is taken from the original `WStr` (so it shows `'1\ud800'`), exactly
        // like CPython, rather than the `TypeError` of the non-string fallback.
        Object::WStr(cps) => {
            let text: String = cps
                .iter()
                .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                .collect();
            parse_int_string(&args[0], &text, &args[1..])
        }
        // bytes-like: each byte maps to one Latin-1 code point so non-ASCII
        // bytes (and embedded NULs) become non-digit characters that fail to
        // parse — with the original `b'…'` repr in the error, like CPython.
        Object::Bytes(b) => {
            let text: String = b.iter().map(|&c| c as char).collect();
            parse_int_string(&args[0], &text, &args[1..])
        }
        Object::ByteArray(b) => {
            let text: String = b.borrow().iter().map(|&c| c as char).collect();
            parse_int_string(&args[0], &text, &args[1..])
        }
        _ => Err(type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            args[0].type_name()
        ))),
    }
}

/// Parse the text of an `int(x, base)` call. `original` is the *original*
/// argument object; its `repr()` is computed lazily and only when an
/// `invalid literal` error is actually raised (so surrounding whitespace and
/// `b'…'` framing are preserved, matching CPython, without paying the O(N)
/// repr cost on the success / digit-limit paths). Unicode decimal digits and
/// whitespace are normalised to ASCII first.
fn parse_int_string(
    original: &Object,
    raw: &str,
    base_arg: &[Object],
) -> Result<Object, RuntimeError> {
    use num_bigint::BigInt;

    // Resolve the base argument up front: the error message reports it
    // verbatim (`base 0`, `base 20`, …), not the prefix-resolved radix.
    let base = if base_arg.is_empty() {
        10u32
    } else {
        match &base_arg[0] {
            Object::Int(i) => u32::try_from(*i)
                .map_err(|_| value_error("int() base must be >= 2 and <= 36, or 0"))?,
            Object::Bool(b) => u32::from(*b),
            Object::Long(_) => return Err(value_error("int() base must be >= 2 and <= 36, or 0")),
            _ => return Err(type_error("int() base must be an integer".to_owned())),
        }
    };
    if base == 1 || base > 36 {
        return Err(value_error("int() base must be >= 2 and <= 36, or 0"));
    }

    // Fast DoS guard (PEP 0467): reject a pathologically long input *before*
    // the O(N) Unicode-normalisation and underscore-stripping passes. A raw
    // string of length L yields at least ceil((L+1)/2) digits once the only
    // legal underscores (between two digits) are removed, so when that lower
    // bound already exceeds the cap the value is over the limit regardless of
    // its exact contents. Power-of-two radices parse in linear time and are
    // exempt, matching CPython.
    let max_digits = crate::stdlib::sys::int_max_str_digits();
    if max_digits > 0 {
        let radix_is_pow2 = base.is_power_of_two()
            || (base == 0 && {
                let t = raw.trim_start();
                let t = t.strip_prefix(['+', '-']).unwrap_or(t);
                let tb = t.as_bytes();
                tb.len() >= 2
                    && tb[0] == b'0'
                    && matches!(tb[1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
            });
        if !radix_is_pow2 && raw.len().div_ceil(2) > max_digits as usize {
            return Err(value_error(format!(
                "Exceeds the limit ({max_digits} digits) for integer string conversion; \
                 use sys.set_int_max_str_digits() to increase the limit"
            )));
        }
    }

    let invalid = || {
        value_error(format!(
            "invalid literal for int() with base {base}: {}",
            original.repr()
        ))
    };

    // Normalise Unicode decimal digits / whitespace to ASCII, then strip the
    // surrounding whitespace CPython ignores.
    let transformed = transform_decimal_and_space(raw);
    let mut s = transformed.trim();
    let mut sign = 1i32;
    if let Some(stripped) = s.strip_prefix('+') {
        s = stripped;
    } else if let Some(stripped) = s.strip_prefix('-') {
        s = stripped;
        sign = -1;
    }

    // Validate underscore placement up front: CPython only accepts a single
    // underscore between two "digit" characters (or right after a base
    // prefix, e.g. `0x_ff`). Leading/trailing/doubled underscores such as
    // `_1`, `1_`, `1__2` are `ValueError`s rather than silently stripped.
    if s.contains('_') {
        let b = s.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'_'
                && !(i > 0
                    && i + 1 < b.len()
                    && b[i - 1].is_ascii_alphanumeric()
                    && b[i + 1].is_ascii_alphanumeric())
            {
                return Err(invalid());
            }
        }
    }

    // Strip a 0x/0o/0b prefix when it matches the base, or pick the
    // base from the prefix when `base == 0`.
    let (radix, digits): (u32, &str) =
        if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            if base == 0 || base == 16 {
                (16, rest)
            } else {
                (base, s)
            }
        } else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
            if base == 0 || base == 8 {
                (8, rest)
            } else {
                (base, s)
            }
        } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
            if base == 0 || base == 2 {
                (2, rest)
            } else {
                (base, s)
            }
        } else if base == 0 {
            (10, s)
        } else {
            (base, s)
        };

    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() {
        return Err(invalid());
    }

    // With base 0 a decimal literal may not carry redundant leading zeros:
    // `int('0', 0)` / `int('00', 0)` are 0, but `int('010', 0)` is invalid
    // (it looks like a defunct octal literal).
    if base == 0 && radix == 10 && cleaned.starts_with('0') && cleaned.bytes().any(|c| c != b'0') {
        return Err(invalid());
    }

    // PEP 0467 int↔str conversion cap. The digit count (sign, whitespace and
    // underscores already stripped) is checked up front — before the O(N**2)
    // big-int parse — so pathological inputs fail fast. Power-of-two radices
    // (linear to parse) are exempt, matching CPython.
    let max_digits = crate::stdlib::sys::int_max_str_digits();
    if max_digits > 0 && !radix.is_power_of_two() && cleaned.len() > max_digits as usize {
        return Err(value_error(format!(
            "Exceeds the limit ({max_digits} digits) for integer string conversion: \
             value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            cleaned.len()
        )));
    }

    if let Ok(small) = i64::from_str_radix(&cleaned, radix) {
        return Ok(Object::Int(if sign < 0 { -small } else { small }));
    }
    let big = BigInt::parse_bytes(cleaned.as_bytes(), radix).ok_or_else(invalid)?;
    let big = if sign < 0 { -big } else { big };
    Ok(Object::int_from_bigint(big))
}

// ---------- int methods (RFC 0019) ----------

fn int_bit_length(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "bit_length")?;
    let n = v.as_bigint().ok_or_else(|| {
        type_error(format!(
            "bit_length: '{}' object is not an integer",
            v.type_name()
        ))
    })?;
    let bits = n.bits();
    Ok(Object::Int(bits as i64))
}

fn int_bit_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "bit_count")?;
    let n = v.as_bigint().ok_or_else(|| {
        type_error(format!(
            "bit_count: '{}' object is not an integer",
            v.type_name()
        ))
    })?;
    // Python: number of 1-bits in the absolute value.
    let abs = n.abs();
    let (_, bytes) = abs.to_bytes_be();
    let count: u32 = bytes.iter().map(|b| b.count_ones()).sum();
    Ok(Object::Int(i64::from(count)))
}

fn int_conjugate(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "conjugate")?;
    Ok(v.clone())
}

fn int_is_integer(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = one(args, "is_integer")?;
    Ok(Object::Bool(true))
}

fn int_as_integer_ratio(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "as_integer_ratio")?;
    // The numerator is a *plain* int even when self is a bool or an int
    // subclass (CPython's long_as_integer_ratio calls _PyLong_Copy;
    // test_long asserts `type(True.as_integer_ratio()[0]) is int`).
    let n = v.as_bigint().ok_or_else(|| {
        type_error(format!(
            "as_integer_ratio: '{}' object is not an integer",
            v.type_name()
        ))
    })?;
    Ok(Object::new_tuple(vec![
        Object::int_from_bigint(n),
        Object::Int(1),
    ]))
}

// CPython signature: `int.to_bytes(length=1, byteorder='big', *, signed=False)`.
// `length`/`byteorder` are positional-or-keyword and `signed` is
// keyword-only, so this must accept keywords (pandas' offsets / hypothesis
// call `n.to_bytes(length, byteorder, signed=...)` by keyword).
fn int_to_bytes(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let n_obj = args
        .first()
        .ok_or_else(|| type_error("to_bytes() requires self"))?;
    let n = n_obj
        .as_bigint()
        .ok_or_else(|| type_error("to_bytes(): self is not an integer"))?;
    let length = match arg_or_kw(args, 1, kwargs, "length") {
        Some(Object::Int(i)) if *i >= 0 => *i as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        Some(Object::Long(b)) if !b.is_negative() => b
            .to_usize()
            .ok_or_else(|| value_error("length out of range"))?,
        None => 1,
        _ => {
            return Err(value_error(
                "length argument must be a non-negative integer",
            ))
        }
    };
    let byteorder = match arg_or_kw(args, 2, kwargs, "byteorder") {
        Some(o) => byteorder_str(o)?,
        None => "big".to_owned(),
    };
    let signed = match arg_or_kw(args, 3, kwargs, "signed") {
        Some(o) => o.is_truthy(),
        None => false,
    };
    let bytes = bigint_to_bytes(&n, length, &byteorder, signed)?;
    Ok(Object::new_bytes(bytes))
}

// CPython signature: `int.from_bytes(bytes, byteorder='big', *, signed=False)`.
// `byteorder` is positional-or-keyword, `signed` keyword-only.
fn int_from_bytes_method(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    // Bound-method form passes self as args[0] (the int class itself
    // in CPython). We treat any int-like first arg as the binding
    // receiver and ignore it.
    let offset = usize::from(
        args.first()
            .map(|o| o.is_int_like() || matches!(o, Object::Type(_)))
            .unwrap_or(false),
    );
    let data_obj = args
        .get(offset)
        .ok_or_else(|| type_error("from_bytes() missing data"))?;
    let data = match data_obj.as_bytes_view() {
        Some(v) => v,
        // A str is iterable but is *not* an acceptable byte source
        // (int.from_bytes("", 'big') is a TypeError, test_long).
        None if matches!(data_obj, Object::Str(_) | Object::WStr(_)) => {
            return Err(type_error("cannot convert 'str' object to bytes"));
        }
        None => {
            // Iterables of ints: collect into bytes; each item must be an
            // int in range(0, 256) — an out-of-range value is a ValueError
            // like `bytes([256])`, never a silent zero.
            let mut it = data_obj
                .make_iter()
                .map_err(|_| type_error("cannot convert non-bytes object to bytes"))?;
            let mut out = Vec::new();
            while let Some(x) = it.next_value() {
                match x {
                    Object::Int(b) if (0..=255).contains(&b) => out.push(b as u8),
                    Object::Bool(b) => out.push(u8::from(b)),
                    Object::Int(_) | Object::Long(_) => {
                        return Err(value_error("bytes must be in range(0, 256)"));
                    }
                    other => {
                        return Err(type_error(format!(
                            "'{}' object cannot be interpreted as an integer",
                            other.type_name_owned()
                        )));
                    }
                }
            }
            out
        }
    };
    let byteorder = match arg_or_kw(args, offset + 1, kwargs, "byteorder") {
        Some(o) => byteorder_str(o)?,
        None => "big".to_owned(),
    };
    let signed = match arg_or_kw(args, offset + 2, kwargs, "signed") {
        Some(o) => o.is_truthy(),
        None => false,
    };
    let n = bytes_to_bigint(&data, &byteorder, signed)?;
    Ok(Object::int_from_bigint(n))
}

/// `byteorder` is parsed with `unicode_compare_eq` in CPython, so any `str`
/// *instance* — including subclasses — is accepted (test_long uses a
/// `SubStr('big')`); everything else is `TypeError`.
fn byteorder_str(o: &Object) -> Result<String, RuntimeError> {
    match o {
        Object::Str(s) => Ok(s.to_string()),
        Object::WStr(cps) => Ok(cps
            .iter()
            .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
            .collect()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::Str(s)) => Ok(s.to_string()),
            _ => Err(type_error("byteorder must be a string")),
        },
        _ => Err(type_error("byteorder must be a string")),
    }
}

fn bigint_to_bytes(
    n: &BigInt,
    length: usize,
    byteorder: &str,
    signed: bool,
) -> Result<Vec<u8>, RuntimeError> {
    if !signed && n.is_negative() {
        return Err(crate::error::overflow_error(
            "can't convert negative int to unsigned",
        ));
    }
    if length == 0 && !n.is_zero() {
        return Err(crate::error::overflow_error("int too big to convert"));
    }
    let bytes = if signed {
        // Zero needs no bytes at all: `(0).to_bytes(0, 'little')` is b''
        // (random.Random.randbytes(0) relies on it), but num-bigint
        // renders zero as [0].
        let raw = if n.is_zero() {
            Vec::new()
        } else {
            n.to_signed_bytes_be()
        };
        if raw.len() > length {
            // CPython raises OverflowError, not ValueError
            // ((256).to_bytes(1, 'big'); test_long.test_to_bytes).
            return Err(crate::error::overflow_error("int too big to convert"));
        }
        let pad_byte = if n.is_negative() { 0xFF } else { 0x00 };
        let mut out = vec![pad_byte; length - raw.len()];
        out.extend_from_slice(&raw);
        out
    } else {
        let raw = if n.is_zero() {
            Vec::new()
        } else {
            n.to_bytes_be().1
        };
        if raw.len() > length {
            return Err(crate::error::overflow_error("int too big to convert"));
        }
        let mut out = vec![0u8; length - raw.len()];
        out.extend_from_slice(&raw);
        out
    };
    match byteorder {
        "big" => Ok(bytes),
        "little" => {
            let mut rev = bytes;
            rev.reverse();
            Ok(rev)
        }
        _ => Err(value_error(
            "byteorder must be either 'little' or 'big'".to_owned(),
        )),
    }
}

fn bytes_to_bigint(data: &[u8], byteorder: &str, signed: bool) -> Result<BigInt, RuntimeError> {
    let buf: Vec<u8> = match byteorder {
        "big" => data.to_vec(),
        "little" => {
            let mut v = data.to_vec();
            v.reverse();
            v
        }
        _ => {
            return Err(value_error(
                "byteorder must be either 'little' or 'big'".to_owned(),
            ))
        }
    };
    if signed {
        Ok(BigInt::from_signed_bytes_be(&buf))
    } else {
        Ok(BigInt::from_bytes_be(num_bigint::Sign::Plus, &buf))
    }
}

// ---------- float methods (RFC 0019) ----------

fn float_is_integer(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "is_integer")?;
    match v {
        Object::Float(f) => Ok(Object::Bool(f.is_finite() && f.fract() == 0.0)),
        _ => Err(type_error("is_integer: float expected")),
    }
}

fn float_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "hex")?;
    match v {
        Object::Float(f) => Ok(Object::from_str(format_float_hex(*f))),
        _ => Err(type_error("hex: float expected")),
    }
}

fn float_fromhex(args: &[Object]) -> Result<Object, RuntimeError> {
    // First arg is the class (float) for classmethod-style; tolerate
    // either form.
    let (cls, s_obj) = if matches!(args.first(), Some(Object::Type(_))) {
        (args.first(), args.get(1))
    } else {
        (None, args.first())
    };
    let s = match s_obj {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("fromhex() requires a string")),
    };
    let x = parse_float_hex(&s)?;
    float_fromhex_wrap(cls, x)
}

/// Wrap a parsed `fromhex` value in the requested class. For the plain
/// `float` type that's just `Object::Float`; for a subclass we re-enter the
/// interpreter and call `cls(x)` so the subclass's `__new__`/`__init__`
/// run (CPython does `PyObject_CallOneArg(type, result)`).
fn float_fromhex_wrap(cls: Option<&Object>, x: f64) -> Result<Object, RuntimeError> {
    if let Some(Object::Type(t)) = cls {
        let bt = crate::builtin_types::builtin_types();
        if !crate::sync::Rc::ptr_eq(t, &bt.float_) {
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error("float.fromhex() subclass construction requires a running interpreter")
            })?;
            // SAFETY: pointer published by the running dispatch loop for this
            // thread; re-entered synchronously like the other reentrant
            // callbacks (`__hash__`, `__eq__`).
            let interp = unsafe { &mut *ptr };
            return interp.call_object(Object::Type(t.clone()), &[Object::Float(x)], &[]);
        }
    }
    Ok(Object::Float(x))
}

fn float_as_integer_ratio(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "as_integer_ratio")?;
    let f = match v {
        Object::Float(f) => *f,
        _ => return Err(type_error("as_integer_ratio: float expected")),
    };
    if f.is_nan() {
        return Err(value_error("cannot convert NaN to integer ratio"));
    }
    if f.is_infinite() {
        return Err(crate::error::overflow_error(
            "cannot convert Infinity to integer ratio",
        ));
    }
    let bits = f.to_bits();
    let sign = if (bits >> 63) & 1 == 1 { -1i32 } else { 1 };
    let exp_field = ((bits >> 52) & 0x7FF) as i32;
    let mantissa_field = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent): (BigInt, i32) = if exp_field == 0 {
        // Subnormal.
        (BigInt::from(mantissa_field), -1074)
    } else {
        let m = (1u64 << 52) | mantissa_field;
        (BigInt::from(m), exp_field - 1075)
    };
    let mut num = mantissa;
    let mut den = BigInt::from(1);
    if exponent >= 0 {
        num <<= exponent as usize;
    } else {
        den <<= (-exponent) as usize;
    }
    use num_integer::Integer;
    let g = num.gcd(&den);
    num /= &g;
    den /= &g;
    if sign < 0 {
        num = -num;
    }
    Ok(Object::new_tuple(vec![
        Object::int_from_bigint(num),
        Object::int_from_bigint(den),
    ]))
}

fn float_conjugate(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(one(args, "conjugate")?.clone())
}

fn float_trunc(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "__trunc__")?;
    match v {
        Object::Float(f) => Ok(Object::int_from_bigint(
            crate::object::bigint_from_f64_trunc(f.trunc()),
        )),
        _ => Err(type_error("__trunc__: float expected")),
    }
}

/// `float.__int__(self)` — truncate toward zero, raising the same errors
/// CPython does for non-finite inputs (`ValueError` for NaN, `OverflowError`
/// for ±inf). Kept behaviourally identical to the type-dict `float.__int__`
/// so instance- and type-level access agree.
fn float_int(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "__int__")? {
        Object::Float(f) => {
            if f.is_nan() {
                return Err(value_error("cannot convert float NaN to integer"));
            }
            if f.is_infinite() {
                return Err(crate::error::overflow_error(
                    "cannot convert float infinity to integer",
                ));
            }
            Ok(Object::int_from_bigint(
                crate::object::bigint_from_f64_trunc(f.trunc()),
            ))
        }
        _ => Err(type_error("__int__: float expected")),
    }
}

fn float_floor(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "__floor__")? {
        Object::Float(f) => float_int_part(f.floor()),
        _ => Err(type_error("__floor__: float expected")),
    }
}

fn float_ceil(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "__ceil__")? {
        Object::Float(f) => float_int_part(f.ceil()),
        _ => Err(type_error("__ceil__: float expected")),
    }
}

/// Convert an already-floored/ceiled `f64` to an `int`, raising the same
/// errors CPython's `float.__floor__`/`__ceil__` do for non-finite values.
fn float_int_part(f: f64) -> Result<Object, RuntimeError> {
    if f.is_nan() {
        return Err(value_error("cannot convert float NaN to integer"));
    }
    if f.is_infinite() {
        return Err(crate::error::overflow_error(
            "cannot convert float infinity to integer",
        ));
    }
    Ok(Object::int_from_bigint(
        crate::object::bigint_from_f64_trunc(f),
    ))
}

fn float_round(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "__round__")?;
    let f = match v {
        Object::Float(f) => *f,
        _ => return Err(type_error("__round__: float expected")),
    };
    let ndigits = match args.get(1) {
        Some(Object::Int(i)) => Some(*i),
        Some(Object::Bool(b)) => Some(i64::from(*b)),
        Some(Object::None) | None => None,
        _ => return Err(type_error("__round__: ndigits must be int or None")),
    };
    if let Some(d) = ndigits {
        let pow = 10f64.powi(d as i32);
        let rounded = (f * pow).round() / pow;
        return Ok(Object::Float(rounded));
    }
    // Banker's rounding (CPython): round half to even.
    let r = f.round_ties_even();
    Ok(Object::int_from_bigint(
        crate::object::bigint_from_f64_trunc(r),
    ))
}

fn format_float_hex(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f == f64::INFINITY {
        return "inf".to_owned();
    }
    if f == f64::NEG_INFINITY {
        return "-inf".to_owned();
    }
    let bits = f.to_bits();
    let sign = (bits >> 63) & 1 == 1;
    let exp_field = ((bits >> 52) & 0x7FF) as i32;
    let mantissa = bits & ((1u64 << 52) - 1);
    if exp_field == 0 && mantissa == 0 {
        return if sign { "-0x0.0p+0" } else { "0x0.0p+0" }.to_owned();
    }
    // CPython's `float_hex` always prints the full 13 hex digits of the
    // 52-bit fraction — `(1/16).hex()` is '0x1.0000000000000p-4', never
    // '0x1.0p-4' (test_random's test_guaranteed_stable compares hex
    // strings verbatim).
    let (m_hex, exponent) = if exp_field == 0 {
        // Subnormal
        (format!("0x0.{mantissa:013x}"), -1022)
    } else {
        (format!("0x1.{mantissa:013x}"), exp_field - 1023)
    };
    let sign_str = if sign { "-" } else { "" };
    let exp_sign = if exponent >= 0 { "+" } else { "" };
    format!("{sign_str}{m_hex}p{exp_sign}{exponent}")
}

/// `float.fromhex` string parser, a faithful port of CPython's
/// `float_fromhex` (`Objects/floatobject.c`). Returns the parsed value
/// (with correct round-half-even in the subnormal range), a `ValueError`
/// for malformed input, or an `OverflowError` for values too large to
/// represent. Works on raw bytes so embedded NULs and multibyte
/// (fullwidth) digits are rejected exactly as CPython rejects them.
fn parse_float_hex(s: &str) -> Result<f64, RuntimeError> {
    const DBL_MANT_DIG: i64 = 53;
    const DBL_MIN_EXP: i64 = -1021;
    const DBL_MAX_EXP: i64 = 1024;
    let parse_err = || value_error("invalid hexadecimal floating-point string");
    let overflow =
        || crate::error::overflow_error("hexadecimal value too large to represent as a float");

    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;

    // Leading whitespace.
    while i < n && is_py_space(bytes[i]) {
        i += 1;
    }

    // Infinities and nans (consume their own optional sign).
    if let Some((val, end)) = parse_inf_or_nan(bytes, i) {
        return finish_hex_tail(bytes, end, val);
    }

    // Optional sign.
    let mut negate = false;
    if i < n && bytes[i] == b'-' {
        negate = true;
        i += 1;
    } else if i < n && bytes[i] == b'+' {
        i += 1;
    }

    // Optional `0x` / `0X` prefix.
    let s_store = i;
    if i < n && bytes[i] == b'0' {
        i += 1;
        if i < n && (bytes[i] == b'x' || bytes[i] == b'X') {
            i += 1;
        } else {
            i = s_store;
        }
    }

    // Coefficient: <integer> [. <fraction>].
    let coeff_start = i;
    while i < n && hex_from_byte(bytes[i]) >= 0 {
        i += 1;
    }
    let dot_store = i;
    let coeff_end: usize = if i < n && bytes[i] == b'.' {
        i += 1;
        while i < n && hex_from_byte(bytes[i]) >= 0 {
            i += 1;
        }
        i - 1
    } else {
        i
    };

    let mut ndigits = coeff_end as i64 - coeff_start as i64;
    let fdigits = coeff_end as i64 - dot_store as i64;
    if ndigits == 0 {
        return Err(parse_err());
    }
    let length_limit = core::cmp::min(
        DBL_MIN_EXP - DBL_MANT_DIG - i64::MIN / 2,
        i64::MAX / 2 + 1 - DBL_MAX_EXP,
    ) / 4;
    if ndigits > length_limit {
        return Err(value_error("hexadecimal string too long to convert"));
    }

    // Optional `p <exponent>`.
    let mut exp: i64 = 0;
    if i < n && (bytes[i] == b'p' || bytes[i] == b'P') {
        i += 1;
        let exp_start = i;
        if i < n && (bytes[i] == b'-' || bytes[i] == b'+') {
            i += 1;
        }
        if !(i < n && bytes[i].is_ascii_digit()) {
            return Err(parse_err());
        }
        i += 1;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // `strtol` saturates to LONG_MIN/MAX on overflow; mirror that so a
        // gigantic exponent funnels into the overflow/zero branches below.
        let exp_text = std::str::from_utf8(&bytes[exp_start..i]).unwrap_or("0");
        exp = exp_text
            .parse::<i64>()
            .unwrap_or(if bytes[exp_start] == b'-' {
                i64::MIN
            } else {
                i64::MAX
            });
    }

    // `HEX_DIGIT(j)` — the j'th least-significant hex digit, hopping over the
    // radix point for digits in the integer part.
    let hex_digit = |j: i64| -> i32 {
        let idx = if j < fdigits {
            coeff_end as i64 - j
        } else {
            coeff_end as i64 - 1 - j
        };
        hex_from_byte(bytes[idx as usize])
    };

    // Discard leading zeros; catch extreme over/underflow.
    while ndigits > 0 && hex_digit(ndigits - 1) == 0 {
        ndigits -= 1;
    }
    if ndigits == 0 || exp < i64::MIN / 2 {
        return finish_hex_tail(bytes, i, if negate { -0.0 } else { 0.0 });
    }
    if exp > i64::MAX / 2 {
        return Err(overflow());
    }

    // Adjust exponent for the fractional part.
    exp -= 4 * fdigits;

    // `top_exp` = one more than the exponent of the most-significant bit.
    let mut top_exp = exp + 4 * (ndigits - 1);
    let mut msd = hex_digit(ndigits - 1);
    while msd != 0 {
        top_exp += 1;
        msd /= 2;
    }

    if top_exp < DBL_MIN_EXP - DBL_MANT_DIG {
        return finish_hex_tail(bytes, i, if negate { -0.0 } else { 0.0 });
    }
    if top_exp > DBL_MAX_EXP {
        return Err(overflow());
    }

    let lsb = core::cmp::max(top_exp, DBL_MIN_EXP) - DBL_MANT_DIG;
    let mut x: f64 = 0.0;
    if exp >= lsb {
        // No rounding required.
        let mut j = ndigits - 1;
        while j >= 0 {
            x = 16.0 * x + f64::from(hex_digit(j));
            j -= 1;
        }
        x = crate::stdlib::math::ldexp(x, exp as i32);
        return finish_hex_tail(bytes, i, if negate { -x } else { x });
    }

    // Rounding required. `key_digit` holds the first bit to round away.
    let half_eps = 1i32 << ((lsb - exp - 1) % 4) as u32;
    let key_digit = (lsb - exp - 1) / 4;
    let mut j = ndigits - 1;
    while j > key_digit {
        x = 16.0 * x + f64::from(hex_digit(j));
        j -= 1;
    }
    let digit = hex_digit(key_digit);
    x = 16.0 * x + f64::from(digit & (16 - 2 * half_eps));

    // Round half to even.
    if (digit & half_eps) != 0 {
        let mut round_up = false;
        if (digit & (3 * half_eps - 1)) != 0
            || (half_eps == 8 && key_digit + 1 < ndigits && (hex_digit(key_digit + 1) & 1) != 0)
        {
            round_up = true;
        } else {
            let mut k = key_digit - 1;
            while k >= 0 {
                if hex_digit(k) != 0 {
                    round_up = true;
                    break;
                }
                k -= 1;
            }
        }
        if round_up {
            x += f64::from(2 * half_eps);
            if top_exp == DBL_MAX_EXP
                && x == crate::stdlib::math::ldexp(f64::from(2 * half_eps), DBL_MANT_DIG as i32)
            {
                // Pre-rounding value was < DBL_MAX, post-rounding == DBL_MAX.
                return Err(overflow());
            }
        }
    }
    x = crate::stdlib::math::ldexp(x, (exp + 4 * key_digit) as i32);
    finish_hex_tail(bytes, i, if negate { -x } else { x })
}

/// CPython `Py_ISSPACE` for the ASCII range (space, tab, newline, vtab,
/// formfeed, carriage return).
fn is_py_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Value of an ASCII hex digit, or `-1` for anything else (including
/// multibyte UTF-8 lead bytes, so fullwidth digits are rejected).
fn hex_from_byte(b: u8) -> i32 {
    match b {
        b'0'..=b'9' => i32::from(b - b'0'),
        b'a'..=b'f' => i32::from(b - b'a' + 10),
        b'A'..=b'F' => i32::from(b - b'A' + 10),
        _ => -1,
    }
}

/// ASCII case-insensitive match of `pat` at `s[i..]`.
fn ci_match(s: &[u8], i: usize, pat: &[u8]) -> bool {
    s.len() >= i + pat.len() && s[i..i + pat.len()].eq_ignore_ascii_case(pat)
}

/// CPython `_Py_parse_inf_or_nan`: parse an optional sign followed by
/// `inf`/`infinity`/`nan` (case-insensitive). Returns the value and the
/// index just past the match, or `None` if no match.
fn parse_inf_or_nan(s: &[u8], start: usize) -> Option<(f64, usize)> {
    let n = s.len();
    let mut i = start;
    let mut negate = false;
    if i < n && s[i] == b'-' {
        negate = true;
        i += 1;
    } else if i < n && s[i] == b'+' {
        i += 1;
    }
    if ci_match(s, i, b"inf") {
        i += 3;
        if ci_match(s, i, b"inity") {
            i += 5;
        }
        Some((
            if negate {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            },
            i,
        ))
    } else if ci_match(s, i, b"nan") {
        i += 3;
        // Fresh identity per parse — CPython allocates a new float object
        // (see `tag_nan` in object.rs).
        let nan = crate::object::tag_nan(if negate { -f64::NAN } else { f64::NAN });
        Some((nan, i))
    } else {
        None
    }
}

/// Skip trailing ASCII whitespace and require we've reached the end of the
/// string (CPython rejects trailing junk, including bytes past an embedded
/// NUL).
fn finish_hex_tail(s: &[u8], mut i: usize, val: f64) -> Result<f64, RuntimeError> {
    let n = s.len();
    while i < n && is_py_space(s[i]) {
        i += 1;
    }
    if i != n {
        return Err(value_error("invalid hexadecimal floating-point string"));
    }
    Ok(val)
}

// ---------- classmethod-shaped wrappers used by builtin_types ----------
//
// These are exposed via the type dict so `int.from_bytes(...)` and
// `bytes.fromhex(...)` resolve correctly. The descriptor protocol
// binds `cls` to args[0], so each helper just discards args[0] and
// routes the rest through the underlying body.

pub(crate) fn b_int_from_bytes_cls(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    // CPython's `int.from_bytes` calls `cls(result)` for subclasses so
    // e.g. `IntEnum.from_bytes(...)` resolves to the matching member.
    let result = int_from_bytes_method(args, kwargs)?;
    fromhex_wrap_subclass(args.first(), "int", result)
}

fn fromhex_string_arg(arg: Option<&Object>) -> Result<String, RuntimeError> {
    match arg {
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(other) => Err(type_error(format!(
            "fromhex() argument must be str, not {}",
            other.type_name()
        ))),
        None => Err(type_error(
            "descriptor 'fromhex' of 'bytes' object needs an argument",
        )),
    }
}

/// CPython's `bytes.fromhex` on a subclass calls the subclass with the
/// parsed result (`PyObject_CallOneArg(type, result)`), so the returned
/// object is an instance of `cls`.
fn fromhex_wrap_subclass(
    cls: Option<&Object>,
    base_name: &str,
    result: Object,
) -> Result<Object, RuntimeError> {
    if let Some(cls_obj @ Object::Type(t)) = cls {
        if t.name != base_name {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame still live on
                // this thread; the GIL keeps the access exclusive.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                return interp.call_object_with_globals(cls_obj, &[result], &[], &globals);
            }
        }
    }
    Ok(result)
}

pub(crate) fn b_bytes_fromhex_cls(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = fromhex_string_arg(args.get(1))?;
    let bytes = parse_hex_bytes(&s)?;
    fromhex_wrap_subclass(args.first(), "bytes", Object::new_bytes(bytes))
}

pub(crate) fn b_bytearray_fromhex_cls(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = fromhex_string_arg(args.get(1))?;
    let bytes = parse_hex_bytes(&s)?;
    fromhex_wrap_subclass(args.first(), "bytearray", Object::new_bytearray(bytes))
}

/// `float.__getformat__(typestr)` — CPython's undocumented IEEE-754 probe
/// (`Objects/floatobject.c float_getformat`). `typestr` must be `"double"`
/// or `"float"`; the result is `"IEEE, little-endian"` /
/// `"IEEE, big-endian"` (Rust f32/f64 are IEEE 754 on all supported
/// targets).
pub(crate) fn b_float_getformat_cls(args: &[Object]) -> Result<Object, RuntimeError> {
    let typestr = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "__getformat__() argument must be string, not {}",
                other.type_name()
            )));
        }
        None => return Err(type_error("__getformat__() missing required argument")),
    };
    if typestr != "double" && typestr != "float" {
        return Err(value_error(
            "__getformat__() argument 1 must be 'double' or 'float'",
        ));
    }
    let endian = if cfg!(target_endian = "little") {
        "IEEE, little-endian"
    } else {
        "IEEE, big-endian"
    };
    Ok(Object::from_str(endian))
}

pub(crate) fn b_float_fromhex_cls(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = args.first();
    let s = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("fromhex() argument must be str")),
    };
    let x = parse_float_hex(&s)?;
    float_fromhex_wrap(cls, x)
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, RuntimeError> {
    // CPython's `_PyBytes_FromHex`: pairs of hex digits, with *ASCII*
    // whitespace permitted only between pairs. Error positions are
    // character offsets into the original string.
    let hex_err = |pos: usize| {
        value_error(format!(
            "non-hexadecimal number found in fromhex() arg at position {pos}"
        ))
    };
    let chars: Vec<char> = s.chars().collect();
    let mut bytes = Vec::with_capacity(chars.len() / 2);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r') {
            i += 1;
            continue;
        }
        let hi = if c.is_ascii() { c.to_digit(16) } else { None }.ok_or_else(|| hex_err(i))?;
        let lo = match chars.get(i + 1) {
            Some(c2) if c2.is_ascii() => c2.to_digit(16).ok_or_else(|| hex_err(i + 1))?,
            _ => return Err(hex_err(i + 1)),
        };
        bytes.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Ok(bytes)
}

// ---------- complex methods (RFC 0019) ----------

fn complex_conjugate(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "conjugate")?;
    match v {
        Object::Complex(c) => Ok(Object::new_complex(c.real, -c.imag)),
        _ => Err(type_error("conjugate: complex expected")),
    }
}

pub(crate) fn b_float_compat(args: &[Object]) -> Result<Object, RuntimeError> {
    b_float(args)
}

fn b_float(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_traits::ToPrimitive;

    if args.is_empty() {
        return Ok(Object::Float(0.0));
    }
    match &args[0] {
        Object::Int(i) => Ok(Object::Float(*i as f64)),
        Object::Long(b) => {
            // CPython raises OverflowError when the magnitude exceeds the
            // f64 range rather than silently producing `inf`.
            match b.to_f64() {
                Some(f) if f.is_finite() => Ok(Object::Float(f)),
                _ => Err(crate::error::overflow_error(
                    "int too large to convert to float",
                )),
            }
        }
        Object::Bool(b) => Ok(Object::Float(f64::from(*b))),
        Object::Float(f) => Ok(Object::Float(*f)),
        Object::Str(_)
        | Object::WStr(_)
        | Object::Bytes(_)
        | Object::ByteArray(_)
        | Object::MemoryView(_) => {
            // str / bytes-like: bytes-like buffers are decoded as ASCII-ish
            // text; non-UTF-8 input simply fails to parse (CPython raises the
            // same ValueError). A WTF-8 `str` (lone surrogates) decodes with
            // each surrogate as U+FFFD, which never parses as a float, so
            // `float('\ud8f0')` raises `ValueError` (repr from the original
            // `WStr`) instead of the non-string `TypeError`.
            let text: Option<String> = match &args[0] {
                Object::Str(s) => Some(s.to_string()),
                Object::WStr(cps) => Some(
                    cps.iter()
                        .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                        .collect(),
                ),
                Object::Bytes(b) => String::from_utf8(b.to_vec()).ok(),
                Object::ByteArray(b) => String::from_utf8(b.borrow().to_vec()).ok(),
                Object::MemoryView(mv) => String::from_utf8(mv.to_bytes()).ok(),
                _ => unreachable!(),
            };
            text.as_deref()
                .and_then(parse_float_text)
                .map(Object::Float)
                .ok_or_else(|| {
                    value_error(format!(
                        "could not convert string to float: {}",
                        args[0].repr()
                    ))
                })
        }
        _ => Err(type_error(format!(
            "float() argument must be a string or a number, not '{}'",
            args[0].type_name()
        ))),
    }
}

/// Parse a `float()` string argument following CPython's grammar: surrounding
/// whitespace is stripped, `inf`/`nan` spellings are accepted, and PEP 515
/// underscores are honoured only *between* digits. Returns `None` on any
/// malformed input (the caller renders the `could not convert` ValueError).
fn parse_float_text(raw: &str) -> Option<f64> {
    let transformed = transform_decimal_and_space(raw);
    let s = transformed.trim();
    if s.is_empty() || !valid_float_underscores(s) {
        return None;
    }
    let cleaned: String = s.chars().filter(|&c| c != '_').collect();
    match cleaned.to_ascii_lowercase().as_str() {
        "inf" | "infinity" | "+inf" | "+infinity" => return Some(f64::INFINITY),
        "-inf" | "-infinity" => return Some(f64::NEG_INFINITY),
        // Fresh identity per parse (CPython allocates a new float object).
        "nan" | "+nan" => return Some(crate::object::tag_nan(f64::NAN)),
        // Preserve the sign bit so `copysign(1.0, float('-nan'))` is -1.0.
        "-nan" => return Some(crate::object::tag_nan(-f64::NAN)),
        _ => {}
    }
    // Reject the bare `inf`/`infinity`/`nan` tokens that Rust's parser also
    // accepts (CPython only takes the spellings handled above); everything
    // else Rust accepts matches CPython's float grammar closely enough.
    if cleaned
        .bytes()
        .any(|b| b.eq_ignore_ascii_case(&b'i') || b.eq_ignore_ascii_case(&b'n'))
    {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// CPython's `_PyUnicode_TransformDecimalAndSpaceToASCII`: map Unicode
/// decimal digits to their ASCII value and any Unicode whitespace to a
/// plain space, so `float("\u0663.\u0661\u0664")` and
/// `float("\N{EM SPACE}3.14")` parse. Any other non-ASCII character becomes
/// `'?'` (and truncates), which makes the subsequent parse fail with the
/// same `ValueError` CPython raises.
fn transform_decimal_and_space(raw: &str) -> String {
    if raw.is_ascii() {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if (c as u32) < 127 {
            out.push(c);
        } else if c.is_whitespace() {
            out.push(' ');
        } else if let Some(v) = unicode_decimal_value(c) {
            out.push((b'0' + v as u8) as char);
        } else {
            out.push('?');
            break;
        }
    }
    out
}

/// Decimal value (0–9) of a Unicode `Nd` (Decimal_Number) character, or
/// `None` — straight from the generated UCD 15.1.0 record.
fn unicode_decimal_value(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    crate::stdlib::ucd::record(c as u32, false)
        .decimal()
        .map(u32::from)
}

/// PEP 515 underscore rule for decimal float literals: every `_` must sit
/// directly between two ASCII digits (so `1_000` is fine but `_1`, `1_`,
/// `1__0`, `1_.0`, `1e_5` are not).
fn valid_float_underscores(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'_'
            && !(i > 0 && b[i - 1].is_ascii_digit() && i + 1 < b.len() && b[i + 1].is_ascii_digit())
        {
            return false;
        }
    }
    true
}

fn b_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "bool expected at most 1 argument, got {}",
            args.len()
        )));
    }
    if args.is_empty() {
        return Ok(Object::Bool(false));
    }
    Ok(Object::Bool(args[0].is_truthy()))
}

/// Coerce a numeric `complex()` argument to f64. An int beyond the finite
/// double range raises OverflowError like CPython's `PyLong_AsDouble`
/// (`complex(1 << 30000)`, test_long.test_float_overflow).
fn complex_num_operand(o: &Object) -> Result<f64, RuntimeError> {
    match o {
        Object::Long(b) => {
            use num_traits::ToPrimitive;
            match b.to_f64() {
                Some(f) if f.is_finite() => Ok(f),
                _ => Err(crate::error::overflow_error(
                    "int too large to convert to float",
                )),
            }
        }
        _ => Ok(o.as_f64().expect("numeric")),
    }
}

pub fn b_complex(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_complex(0.0, 0.0));
    }
    let has_second = args.len() >= 2;
    // CPython's `complex_new` ordering: a string `real` is only valid as the
    // sole argument; a string `imag` is never valid. Both checks precede the
    // numeric coercion (so e.g. `complex({}, '1')` reports the string, not the
    // dict).
    // A WTF-8 `str` (lone surrogates, stored as `WStr`) counts as a string
    // here just like `Str`: `complex('\ud800')` is a malformed-string
    // `ValueError`, not a `TypeError`, matching CPython.
    let str_first: Option<String> = match &args[0] {
        Object::Str(s) => Some(s.to_string()),
        Object::WStr(cps) => Some(
            cps.iter()
                .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                .collect(),
        ),
        _ => None,
    };
    if let Some(s) = str_first {
        if has_second {
            return Err(type_error(
                "complex() can't take second arg if first is a string",
            ));
        }
        return parse_complex_string(&s).map(|(r, i)| Object::new_complex(r, i));
    }
    if has_second && matches!(&args[1], Object::Str(_) | Object::WStr(_)) {
        return Err(type_error("complex() second arg can't be a string"));
    }
    let real = match &args[0] {
        Object::Complex(c) => {
            return Ok(args.get(1).cloned().map_or_else(
                || Object::Complex(c.clone()),
                |b| {
                    let bc = b.as_complex().unwrap_or((0.0, 0.0));
                    Object::new_complex(c.real - bc.1, c.imag + bc.0)
                },
            ))
        }
        Object::Int(_) | Object::Long(_) | Object::Bool(_) | Object::Float(_) => {
            complex_num_operand(&args[0])?
        }
        other => {
            return Err(type_error(format!(
                "complex() first argument must be a string or a number, not '{}'",
                other.type_name_owned()
            )));
        }
    };
    let imag = if let Some(b) = args.get(1) {
        match b {
            Object::Complex(c) => return Ok(Object::new_complex(real - c.imag, c.real)),
            Object::Int(_) | Object::Long(_) | Object::Bool(_) | Object::Float(_) => {
                complex_num_operand(b)?
            }
            other => {
                return Err(type_error(format!(
                    "complex() second argument must be a number, not '{}'",
                    other.type_name_owned()
                )));
            }
        }
    } else {
        0.0
    };
    Ok(Object::new_complex(real, imag))
}

/// Parse a `complex(str)` argument, following CPython's
/// `complex_from_string_inner` grammar exactly:
///
/// ```text
///   <float>                  - real part only
///   <float>j                 - imaginary part only
///   <float><signed-float>j   - real and imaginary parts
///   <sign>j | j              - bare ±1j
/// ```
///
/// with an optional pair of `repr()` parentheses, leading/trailing
/// whitespace, and PEP 515 underscores (only between digits). Anything
/// else — trailing garbage, a real part with no `j`, doubled signs,
/// embedded NULs — is a `ValueError`.
fn parse_complex_string(s: &str) -> Result<(f64, f64), RuntimeError> {
    let malformed = || value_error("complex() arg is a malformed string");
    // Fold Unicode whitespace to ASCII space (CPython's
    // `_PyUnicode_TransformDecimalAndSpaceToASCII`); non-ASCII, non-space
    // characters are left to fail the parse below, exactly as CPython does.
    let transformed: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let cleaned = strip_number_underscores(&transformed).ok_or_else(malformed)?;
    parse_complex_inner(&cleaned).ok_or_else(malformed)
}

/// Remove PEP 515 underscores from a numeric literal, validating that
/// each `_` sits directly between two ASCII digits. Returns `None` for a
/// misplaced underscore (leading/trailing/doubled/adjacent to a sign,
/// dot, exponent, or `j`).
fn strip_number_underscores(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let prev = if i > 0 { chars[i - 1] } else { '\0' };
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            if !(prev.is_ascii_digit() && next.is_ascii_digit()) {
                return None;
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Scan the longest valid C-`double` prefix of `b` (CPython's
/// `PyOS_string_to_double`): optional sign, then `inf`/`infinity`/`nan`
/// or a decimal mantissa with optional fraction and exponent. Returns
/// `(value, bytes_consumed)`, or `None` when no float prefix is present.
fn parse_double_prefix(b: &[u8]) -> Option<(f64, usize)> {
    let n = b.len();
    let mut i = 0;
    if i < n && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let rest = &b[i..];
    let starts =
        |word: &[u8]| rest.len() >= word.len() && rest[..word.len()].eq_ignore_ascii_case(word);
    let finish = |end: usize| -> Option<(f64, usize)> {
        let slice = std::str::from_utf8(&b[..end]).ok()?;
        slice
            .parse::<f64>()
            .ok()
            .map(|v| (crate::object::tag_nan(v), end))
    };
    if starts(b"infinity") {
        return finish(i + 8);
    }
    if starts(b"inf") {
        return finish(i + 3);
    }
    if starts(b"nan") {
        return finish(i + 3);
    }
    let mut has_digits = false;
    while i < n && b[i].is_ascii_digit() {
        i += 1;
        has_digits = true;
    }
    if i < n && b[i] == b'.' {
        i += 1;
        while i < n && b[i].is_ascii_digit() {
            i += 1;
            has_digits = true;
        }
    }
    if !has_digits {
        return None;
    }
    if i < n && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < n && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < n && b[j].is_ascii_digit() {
            while j < n && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
        // No exponent digits ⇒ stop before the `e` (e.g. "1e1ej").
    }
    finish(i)
}

/// The core of [`parse_complex_string`], operating on an
/// underscore-stripped, whitespace-normalized string. Mirrors CPython's
/// `complex_from_string_inner` state machine; returns `None` on any
/// malformed input.
fn parse_complex_inner(s: &str) -> Option<(f64, f64)> {
    let b = s.as_bytes();
    let len = b.len();
    let mut i = 0;
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    while i < len && is_space(b[i]) {
        i += 1;
    }
    let mut got_bracket = false;
    if i < len && b[i] == b'(' {
        got_bracket = true;
        i += 1;
        while i < len && is_space(b[i]) {
            i += 1;
        }
    }
    let (mut x, mut y) = (0.0_f64, 0.0_f64);
    match parse_double_prefix(&b[i..]) {
        Some((z, consumed)) => {
            i += consumed;
            if i < len && (b[i] == b'+' || b[i] == b'-') {
                x = z;
                match parse_double_prefix(&b[i..]) {
                    Some((yy, c2)) => {
                        y = yy;
                        i += c2;
                    }
                    None => {
                        y = if b[i] == b'+' { 1.0 } else { -1.0 };
                        i += 1;
                    }
                }
                if !(i < len && (b[i] == b'j' || b[i] == b'J')) {
                    return None;
                }
                i += 1;
            } else if i < len && (b[i] == b'j' || b[i] == b'J') {
                i += 1;
                y = z;
            } else {
                x = z;
            }
        }
        None => {
            // No leading float ⇒ must be `<sign>j` or bare `j`.
            if i < len && (b[i] == b'+' || b[i] == b'-') {
                y = if b[i] == b'+' { 1.0 } else { -1.0 };
                i += 1;
            } else {
                y = 1.0;
            }
            if !(i < len && (b[i] == b'j' || b[i] == b'J')) {
                return None;
            }
            i += 1;
        }
    }
    while i < len && is_space(b[i]) {
        i += 1;
    }
    if got_bracket {
        if !(i < len && b[i] == b')') {
            return None;
        }
        i += 1;
        while i < len && is_space(b[i]) {
            i += 1;
        }
    }
    if i != len {
        return None;
    }
    Some((x, y))
}

fn b_list(args: &[Object]) -> Result<Object, RuntimeError> {
    let out = if args.is_empty() {
        Vec::new()
    } else {
        let mut it = args[0].make_iter()?;
        let mut out = Vec::new();
        while let Some(v) = it.next_value() {
            out.push(v);
        }
        out
    };
    let obj = Object::new_list(out);
    // CPython tracks every list; keep `list(...)` consistent with the
    // `[]` literal path so `gc.is_tracked` and cycle collection agree.
    crate::gc_trace::track(obj.clone());
    // tracemalloc parity with the `[]` literal path
    // (`test_tracemalloc.test_reset_peak` builds `list(range(100000))`
    // and expects the peak to reflect it).
    if crate::stdlib::tracemalloc_real::is_tracking() {
        crate::stdlib::tracemalloc_real::track_new_object(&obj);
    }
    Ok(obj)
}

fn b_tuple(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_tuple(Vec::new()));
    }
    // `tuple(t)` on an exact tuple returns `t` itself (CPython reuses the
    // immutable object; `copy.copy(partial).args is partial.args` relies
    // on the identity).
    if let Object::Tuple(_) = &args[0] {
        return Ok(args[0].clone());
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_tuple(out))
}

fn b_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        let obj = Object::new_dict();
        crate::gc_trace::track(obj.clone());
        return Ok(obj);
    }
    if args.len() > 1 {
        return Err(type_error(format!(
            "dict expected at most 1 argument, got {}",
            args.len()
        )));
    }
    // Fast path: another built-in dict copies entry-for-entry. Avoids
    // re-iterating as a sequence of pairs (which would fail, since
    // iter(dict) yields keys, not items).
    if let Object::Dict(src) = &args[0] {
        let mut d = DictData::default();
        for (k, v) in src.borrow().iter() {
            d.insert(k.clone(), v.clone());
        }
        let obj = Object::Dict(Rc::new(RefCell::new(d)));
        crate::gc_trace::track(obj.clone());
        return Ok(obj);
    }
    // Mapping path for user-defined classes (`__keys__` style) is
    // handled by the VM before dispatching here — see
    // `Vm::do_dict_call`. Anything left over is an iterable of pairs.
    let mut it = args[0].make_iter()?;
    let mut d = DictData::default();
    let mut i = 0usize;
    while let Some(pair) = it.next_value() {
        // CPython `PyDict_MergeFromSeq2`: each element must itself be a
        // sequence of exactly two items (a 2-char string works too).
        // User instances iterate through the live interpreter (their
        // `__iter__`/`__getitem__` is Python code).
        let kv: Vec<Object> = if matches!(pair, Object::Instance(_)) {
            let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
                return Err(type_error(format!(
                    "cannot convert dictionary update sequence element #{i} to a sequence"
                )));
            };
            // SAFETY: published by an enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            interp.collect_iterable(&pair, &globals).map_err(|e| {
                if crate::is_type_error(&e) {
                    type_error(format!(
                        "cannot convert dictionary update sequence element #{i} to a sequence"
                    ))
                } else {
                    e
                }
            })?
        } else {
            let mut inner = pair.make_iter().map_err(|_| {
                type_error(format!(
                    "cannot convert dictionary update sequence element #{i} to a sequence"
                ))
            })?;
            let mut kv = Vec::with_capacity(2);
            while let Some(v) = inner.next_value() {
                kv.push(v);
                if kv.len() > 2 {
                    break;
                }
            }
            kv
        };
        if kv.len() != 2 {
            return Err(value_error(format!(
                "dictionary update sequence element #{i} has length {}; 2 is required",
                kv.len()
            )));
        }
        let mut kv = kv.into_iter();
        d.insert(DictKey(kv.next().unwrap()), kv.next().unwrap());
        i += 1;
    }
    let obj = Object::Dict(Rc::new(RefCell::new(d)));
    crate::gc_trace::track(obj.clone());
    Ok(obj)
}

fn b_type(args: &[Object]) -> Result<Object, RuntimeError> {
    // `type(name, bases, ns)` is intercepted by the VM call site
    // (see `Vm::dynamic_type_call`); only the 1-arg form reaches us.
    let arg = one(args, "type")?;
    Ok(Object::Type(class_of(arg)))
}

fn b_set(args: &[Object]) -> Result<Object, RuntimeError> {
    // `set()` takes at most one positional argument (the iterable);
    // `set([], 2)` is a `TypeError` (CPython `set_init`, test_new_or_init).
    if args.len() > 1 {
        return Err(type_error(format!(
            "set expected at most 1 argument, got {}",
            args.len()
        )));
    }
    let out = if args.is_empty() {
        crate::object::SetData::default()
    } else {
        // `set([[]])` raises `TypeError: unhashable type: 'list'` — check
        // each element as it is admitted, like CPython's `set_init`. A
        // custom `__eq__`/`__hash__` that raises during a collision aborts
        // construction (test_badcmp `set([BadCmp(), BadCmp()])`).
        let mut it = args[0].make_iter()?;
        let mut out = crate::object::SetData::default();
        while let Some(v) = it.next_value() {
            let key = set_insert_key(&v)?;
            crate::object::key_cmp_scope(|| out.insert(key))?;
        }
        out
    };
    let obj = Object::Set(Rc::new(RefCell::new(out)));
    // CPython tracks every set (`gc.is_tracked(set())` is True).
    crate::gc_trace::track(obj.clone());
    Ok(obj)
}

fn b_frozenset(args: &[Object]) -> Result<Object, RuntimeError> {
    // `frozenset()` takes at most one positional argument (CPython
    // `frozenset_new`); `frozenset([], 2)` is a `TypeError`.
    if args.len() > 1 {
        return Err(type_error(format!(
            "frozenset expected at most 1 argument, got {}",
            args.len()
        )));
    }
    if args.is_empty() {
        return Ok(Object::new_frozenset_from(Vec::new()));
    }
    // `frozenset(f)` where `f` is *already* an exact frozenset returns `f`
    // itself — CPython `frozenset_new` short-circuits on `PyFrozenSet_CheckExact`
    // so the immutable object is shared (test_constructor_identity). A
    // frozenset *subclass* instance is not exact, so it still builds a fresh
    // frozenset.
    if let Object::FrozenSet(_) = &args[0] {
        return Ok(args[0].clone());
    }
    let mut it = args[0].make_iter()?;
    let mut out = crate::object::SetData::default();
    while let Some(v) = it.next_value() {
        let key = set_insert_key(&v)?;
        crate::object::key_cmp_scope(|| out.insert(key))?;
    }
    Ok(Object::FrozenSet(Rc::new(
        crate::object::FrozenSetObj::new(out),
    )))
}

/// One item of a `bytes(iterable)` source: an integer in
/// `range(0, 256)` via the `__index__` protocol.
fn byte_item_value(o: &Object) -> Result<u8, RuntimeError> {
    let native = o.native_value();
    match native.as_ref().unwrap_or(o) {
        Object::Bool(b) => Ok(u8::from(*b)),
        Object::Int(i) if (0..=255).contains(i) => Ok(*i as u8),
        Object::Int(_) | Object::Long(_) => Err(value_error("bytes must be in range(0, 256)")),
        inst @ Object::Instance(_) => {
            let v = coerce_index_i64(inst)?;
            if (0..=255).contains(&v) {
                Ok(v as u8)
            } else {
                Err(value_error("bytes must be in range(0, 256)"))
            }
        }
        other => Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

/// The non-string source conversion shared by `bytes(x)` and
/// `bytearray(x)` — CPython's `PyBytes_FromObject` /
/// `bytearray_init` tail: index-sized count, buffer copy, or
/// iterable of byte values.
fn bytes_from_source_obj(src: &Object, type_name: &str) -> Result<Vec<u8>, RuntimeError> {
    let zero_fill = |n: i64| -> Result<Vec<u8>, RuntimeError> {
        if n < 0 {
            return Err(value_error("negative count"));
        }
        let mut v = Vec::new();
        v.try_reserve_exact(n as usize).map_err(|_| {
            RuntimeError::PyException(crate::error::PyException::from_builtin(
                "MemoryError",
                String::new(),
            ))
        })?;
        v.resize(n as usize, 0);
        Ok(v)
    };
    match src {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::MemoryView(mv) => {
            // `bytes(m)` on a released view refuses like every other
            // access (test_memoryview._check_released).
            if mv.released.get() {
                return Err(value_error(
                    "operation forbidden on released memoryview object",
                ));
            }
            Ok(mv.to_bytes())
        }
        Object::Bool(b) => zero_fill(i64::from(*b)),
        Object::Int(n) => zero_fill(*n),
        Object::Long(_) => Err(crate::error::overflow_error(
            "cannot fit 'int' into an index-sized integer",
        )),
        Object::List(items) => {
            // CPython re-checks the list length every iteration
            // (gh-34973): an item's `__index__` may mutate the list.
            let cell = items.clone();
            let mut out = Vec::new();
            let mut i = 0usize;
            loop {
                let item = {
                    let l = cell.borrow();
                    if i >= l.len() {
                        break;
                    }
                    l[i].clone()
                };
                out.push(byte_item_value(&item)?);
                i += 1;
            }
            Ok(out)
        }
        Object::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                out.push(byte_item_value(item)?);
            }
            Ok(out)
        }
        Object::Instance(inst) => {
            // `__bytes__` is consulted by `bytes()` only — CPython's
            // bytearray skips straight to the count/buffer/iterable
            // protocol.
            if type_name == "bytes" {
                if let Some(method) = crate::instance_method(src, "__bytes__") {
                    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                        // SAFETY: published by an enclosing VM frame still
                        // live on this thread; the GIL keeps it exclusive.
                        let interp = unsafe { &mut *ptr };
                        let globals = interp.builtins_dict();
                        let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                        return bytes_argview(&r).map_err(|_| {
                            type_error(format!(
                                "__bytes__ returned non-bytes (type {})",
                                r.type_name()
                            ))
                        });
                    }
                }
            }
            // The `__index__` protocol: a TypeError raised *by* the
            // hook falls through to the buffer/iterable path
            // (gh-29159); any other exception propagates (gh-34974).
            let indexable = inst
                .native
                .get()
                .map(|n| n.as_i64().is_some())
                .unwrap_or(false)
                || crate::instance_method(src, "__index__").is_some();
            if indexable {
                match coerce_index_i64(src) {
                    Ok(n) => return zero_fill(n),
                    Err(RuntimeError::PyException(e)) if e.type_name() == "TypeError" => {}
                    Err(other) => return Err(other),
                }
            }
            // Buffer protocol: a bytes/bytearray subclass instance
            // carries its payload natively.
            if let Some(native) = inst.native.get() {
                if matches!(
                    native,
                    Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
                ) {
                    return bytes_from_source_obj(&native.clone(), type_name);
                }
            }
            // PEP 688 buffer protocol: a pure-Python object that exposes
            // `__buffer__` (e.g. `array.array`) is copied byte-for-byte from
            // its buffer, exactly like CPython's `bytes()` — which prefers the
            // buffer protocol over iteration, so `bytes(array('I', ...))` yields
            // the raw machine bytes rather than the (out-of-range) int items.
            if let Some(method) = crate::instance_method(src, "__buffer__") {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still live on
                    // this thread; the GIL keeps the access exclusive.
                    let interp = unsafe { &mut *ptr };
                    let globals = interp.builtins_dict();
                    let view = interp.call_object_with_globals(
                        &method,
                        &[Object::Int(0)],
                        &[],
                        &globals,
                    )?;
                    if let Some(bytes) = view.as_bytes_view() {
                        return Ok(bytes);
                    }
                }
            }
            // C-level buffer protocol: a faithful C instance whose type fills
            // `bf_getbuffer` (numpy's `ndarray`, Cython memoryviews). CPython's
            // `bytes()` prefers the buffer over iteration, so `bytes(ndarray)`
            // is the raw C-order bytes, not per-item ints (pandas'
            // `test_byteswap` feeds `bytes(uint8_array)` to its readers).
            if crate::foreign::is_installed() {
                if let Ok(Object::MemoryView(mv)) = crate::foreign::get_buffer_obj(src) {
                    return Ok(mv.to_bytes());
                }
            }
            // Iterable (including legacy `__getitem__` sequences) via
            // interpreter reentry; `__iter__` exceptions propagate.
            let iterable = crate::instance_method(src, "__iter__").is_some()
                || crate::instance_method(src, "__getitem__").is_some()
                || inst.native.get().is_some();
            if !iterable {
                return Err(type_error(format!(
                    "cannot convert '{}' object to {}",
                    src.type_name(),
                    type_name
                )));
            }
            bytes_from_iterable_reentrant(src, type_name)
        }
        other => {
            // A foreign integer (`bytes(np.int64(3))`) zero-fills like a VM
            // int; a foreign buffer exporter yields its raw bytes (see the
            // Instance arm). Both take priority over iteration, as in CPython.
            if let Object::Foreign(soul) = other {
                if let Ok(n) = crate::foreign::as_index(soul).and_then(|o| {
                    o.as_i64()
                        .ok_or_else(|| type_error("index did not fit in i64"))
                }) {
                    return zero_fill(n);
                }
                if let Ok(Object::MemoryView(mv)) = crate::foreign::get_buffer(soul) {
                    return Ok(mv.to_bytes());
                }
            }
            // Probe iterability *without* consuming the source: for a
            // generator, `make_iter` materialises the whole thing through
            // the live interpreter — probing first would exhaust it, and
            // the real collection below would then see zero items
            // (`bytes(x for x in …)` returned `b''`). Generators are
            // always iterable; everything else gets the cheap probe.
            if !matches!(other, Object::Generator(_)) && other.make_iter().is_err() {
                return Err(type_error(format!(
                    "cannot convert '{}' object to {}",
                    other.type_name(),
                    type_name
                )));
            }
            bytes_from_iterable_reentrant(other, type_name)
        }
    }
}

/// Iterate any object through the running interpreter (generators,
/// sets, user iterables) collecting byte values.
fn bytes_from_iterable_reentrant(src: &Object, type_name: &str) -> Result<Vec<u8>, RuntimeError> {
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        let items = interp.collect_iterable(src, &globals)?;
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            out.push(byte_item_value(item)?);
        }
        Ok(out)
    } else {
        let mut it = src.make_iter().map_err(|_| {
            type_error(format!(
                "cannot convert '{}' object to {}",
                src.type_name(),
                type_name
            ))
        })?;
        let mut out = Vec::new();
        while let Some(v) = it.next_value() {
            out.push(byte_item_value(&v)?);
        }
        Ok(out)
    }
}

/// Shared `bytes(...)` / `bytearray(...)` construction — CPython's
/// `bytes_new_impl` / `bytearray_init` argument handling, including
/// the `encoding` / `errors` keyword rules.
fn bytes_construct(
    args: &[Object],
    kwargs: &[(String, Object)],
    type_name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    if args.len() > 3 {
        return Err(type_error(format!(
            "{type_name}() takes at most 3 arguments ({} given)",
            args.len()
        )));
    }
    let mut source_obj = args.first().cloned();
    let mut encoding_obj = args.get(1).cloned();
    let mut errors_obj = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "source" => source_obj = Some(v.clone()),
            "encoding" => encoding_obj = Some(v.clone()),
            "errors" => errors_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "{type_name}() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let encoding = match &encoding_obj {
        None => None,
        Some(Object::Str(s)) => Some(s.to_string()),
        Some(o) => {
            return Err(type_error(format!(
                "{type_name}() argument 'encoding' must be str, not {}",
                o.type_name()
            )))
        }
    };
    let errors = match &errors_obj {
        None => None,
        Some(Object::Str(s)) => Some(s.to_string()),
        Some(o) => {
            return Err(type_error(format!(
                "{type_name}() argument 'errors' must be str, not {}",
                o.type_name()
            )))
        }
    };
    let Some(src) = source_obj.as_ref() else {
        if encoding.is_some() {
            return Err(type_error("encoding without a string argument"));
        }
        if errors.is_some() {
            return Err(type_error("errors without a string argument"));
        }
        return Ok(Vec::new());
    };
    // String sources require an encoding; non-string sources reject one.
    let as_str: Option<Rc<str>> = match src {
        Object::Str(s) => Some(s.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(s) = as_str {
        let Some(enc) = encoding else {
            return Err(type_error("string argument without an encoding"));
        };
        return crate::stdlib::codecs_mod::encode_str(
            &s,
            &enc,
            errors.as_deref().unwrap_or("strict"),
        );
    }
    if encoding.is_some() {
        return Err(type_error("encoding without a string argument"));
    }
    if errors.is_some() {
        return Err(type_error("errors without a string argument"));
    }
    bytes_from_source_obj(src, type_name)
}

fn b_bytes_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // `bytes(b'…')` with the exact type returns the argument unchanged
    // (immutable, so identity is shareable — `test_repeat_id_preserving`
    // relies on `bytes(x) is x` style sharing).
    if args.len() == 1 && kwargs.is_empty() {
        if let Object::Bytes(b) = &args[0] {
            return Ok(Object::Bytes(b.clone()));
        }
    }
    // CPython `bytes_new_impl`: with no encoding/errors, `__bytes__` is
    // consulted *before* the str complaint — so a str subclass defining
    // `__bytes__` converts through it (issue #25766) — and a bytes-subclass
    // result is returned as-is (issue #24731).
    let mut source_obj = args.first().cloned();
    let mut has_encoding = args.len() > 1;
    let mut has_errors = args.len() > 2;
    for (k, v) in kwargs {
        match k.as_str() {
            "source" => source_obj = Some(v.clone()),
            "encoding" => has_encoding = true,
            "errors" => has_errors = true,
            _ => {}
        }
    }
    if !has_encoding && !has_errors {
        if let Some(src @ Object::Instance(_)) = &source_obj {
            if let Some(method) = crate::instance_method(src, "__bytes__") {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still live
                    // on this thread; the GIL keeps the access exclusive.
                    let interp = unsafe { &mut *ptr };
                    let globals = interp.builtins_dict();
                    let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                    let is_bytes = matches!(&r, Object::Bytes(_))
                        || matches!(&r, Object::Instance(inst)
                            if matches!(inst.native.get(), Some(Object::Bytes(_))));
                    if !is_bytes {
                        return Err(type_error(format!(
                            "__bytes__ returned non-bytes (type {})",
                            r.type_name()
                        )));
                    }
                    return Ok(r);
                }
            }
        }
    }
    Ok(Object::new_bytes(bytes_construct(args, kwargs, "bytes")?))
}

fn b_bytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = b_bytes_kw(args, &[])?;
    if crate::stdlib::tracemalloc_real::is_tracking() {
        crate::stdlib::tracemalloc_real::track_new_object(&obj);
    }
    Ok(obj)
}

fn b_bytearray_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let obj = Object::new_bytearray(bytes_construct(args, kwargs, "bytearray")?);
    if crate::stdlib::tracemalloc_real::is_tracking() {
        crate::stdlib::tracemalloc_real::track_new_object(&obj);
    }
    Ok(obj)
}

fn b_bytearray(args: &[Object]) -> Result<Object, RuntimeError> {
    b_bytearray_kw(args, &[])
}

/// Keyword-argument-aware wrapper for `open`. CPython's signature is
/// `open(file, mode='r', buffering=-1, encoding=None, errors=None,
/// newline=None, closefd=True, opener=None)`. We honour the positional
/// arguments and silently accept the keyword-only ones — encoding /
/// errors / newline are not yet plumbed through (text mode uses UTF-8
/// strict by default), so the kwargs are taken into the bag but
/// ignored unless they would change behaviour we do support.
fn b_open_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // Reuse the positional path. We fold the known keyword arguments into
    // their positional slots (`open(file, mode, buffering, encoding, errors,
    // newline, closefd, opener)`) and accept (but ignore) the rest.
    let mut combined: Vec<Object> = args.to_vec();
    let set_slot = |combined: &mut Vec<Object>, idx: usize, val: Object| {
        while combined.len() <= idx {
            combined.push(Object::None);
        }
        combined[idx] = val;
    };
    for (k, v) in kwargs {
        match k.as_str() {
            "mode" => set_slot(&mut combined, 1, v.clone()),
            "buffering" => set_slot(&mut combined, 2, v.clone()),
            "encoding" => set_slot(&mut combined, 3, v.clone()),
            "errors" => set_slot(&mut combined, 4, v.clone()),
            "newline" => set_slot(&mut combined, 5, v.clone()),
            // `closefd` *is* honoured (positional slot 6): `open(fd, ...,
            // closefd=False)` must hand back a stream that releases the
            // descriptor without closing it. `multiprocessing.popen_spawn_posix`
            // writes the child's pickle through `open(parent_w, 'wb',
            // closefd=False)` and then closes that fd itself via a `Finalize`,
            // so dropping the flag double-closes `parent_w` (EBADF).
            "closefd" => set_slot(&mut combined, 6, v.clone()),
            "opener" => {
                // Accepted but not plumbed through the positional builtin path.
            }
            other => {
                return Err(type_error(format!(
                    "open() got an unexpected keyword argument '{other}'"
                )));
            }
        }
    }
    b_open(&combined)
}

/// Resolve `open()`'s `file` argument. Accepts `str`, `bytes`/`bytearray`,
/// and any `os.PathLike` (via `__fspath__`). Returns the display name (for
/// the file's Python-visible `name`), whether the original value was
/// `bytes`, and the `OsString` actually handed to the OS — for bytes paths
/// the *raw* bytes reach the syscall (CPython passes them through
/// unmodified; on macOS the kernel then rejects undecodable names with
/// EILSEQ, which `test_sqlite3`'s undecodable-path tests rely on to skip).
fn open_path_arg(obj: &Object) -> Result<(String, bool, std::ffi::OsString), RuntimeError> {
    fn from_bytes(b: &[u8]) -> (String, bool, std::ffi::OsString) {
        #[cfg(unix)]
        let os = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(b).to_owned()
        };
        #[cfg(not(unix))]
        let os = std::ffi::OsString::from(String::from_utf8_lossy(b).into_owned());
        (String::from_utf8_lossy(b).into_owned(), true, os)
    }
    fn from_str(s: String) -> (String, bool, std::ffi::OsString) {
        let os = std::ffi::OsString::from(&s);
        (s, false, os)
    }
    match obj {
        Object::Str(s) => Ok(from_str(s.to_string())),
        Object::Bytes(b) => Ok(from_bytes(b)),
        Object::ByteArray(b) => Ok(from_bytes(&b.borrow())),
        Object::Instance(_) => {
            // `os.PathLike`: call `__fspath__()` through the interpreter.
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("open() argument 'file' must be str"))?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let fspath = interp.load_attr_public(obj, "__fspath__").map_err(|_| {
                type_error(format!(
                    "expected str, bytes or os.PathLike object, not {}",
                    obj.type_name()
                ))
            })?;
            let resolved = interp.call_object(fspath, &[], &[])?;
            match resolved {
                Object::Str(s) => Ok(from_str(s.to_string())),
                // PEP 383: a surrogate-bearing str result (pathlib over a
                // surrogateescape'd name) fsencodes to the raw bytes for
                // the syscall while keeping the str flavour for messages.
                Object::WStr(cps) => {
                    let bytes = crate::stdlib::codecs_mod::encode_codepoints(
                        &cps,
                        "utf-8",
                        "surrogateescape",
                    )?;
                    #[cfg(unix)]
                    let os = {
                        use std::os::unix::ffi::OsStrExt;
                        std::ffi::OsStr::from_bytes(&bytes).to_owned()
                    };
                    #[cfg(not(unix))]
                    let os = std::ffi::OsString::from(String::from_utf8_lossy(&bytes).into_owned());
                    Ok((String::from_utf8_lossy(&bytes).into_owned(), false, os))
                }
                Object::Bytes(b) => Ok(from_bytes(&b)),
                other => Err(type_error(format!(
                    "expected __fspath__() to return str or bytes, not {}",
                    other.type_name()
                ))),
            }
        }
        _ => Err(type_error(format!(
            "expected str, bytes or os.PathLike object, not {}",
            obj.type_name()
        ))),
    }
}

/// Validate an `open()` mode string the way CPython's `io.open` /
/// `_io.FileIO` do, raising `ValueError`/`TypeError` *before* the
/// filesystem is touched. Returns whether the mode is binary so callers
/// can apply the "binary mode doesn't take an encoding argument" rule.
pub(crate) fn validate_open_mode(mode: &str) -> Result<bool, RuntimeError> {
    use std::collections::BTreeSet;
    let mut modes: BTreeSet<char> = BTreeSet::new();
    for ch in mode.chars() {
        if !"xrwab+t".contains(ch) {
            return Err(value_error(format!("invalid mode: '{mode}'")));
        }
        modes.insert(ch);
    }
    // A repeated flag (e.g. "rr") has more chars than the deduped set.
    if mode.chars().count() > modes.len() {
        return Err(value_error(format!("invalid mode: '{mode}'")));
    }
    let creating = modes.contains(&'x');
    let reading = modes.contains(&'r');
    let writing = modes.contains(&'w');
    let appending = modes.contains(&'a');
    let text = modes.contains(&'t');
    let binary = modes.contains(&'b');
    if text && binary {
        return Err(value_error("can't have text and binary mode at once"));
    }
    if u8::from(creating) + u8::from(reading) + u8::from(writing) + u8::from(appending) > 1 {
        // CPython's `_io.open` (and `_pyio.open`) word this exactly as
        // "can't have read/write/append mode at once" — `test_io`
        // (`test_fspath_support`) matches the message against
        // `'read/write/append mode'`.
        return Err(value_error("can't have read/write/append mode at once"));
    }
    if !(creating || reading || writing || appending) {
        return Err(value_error(
            "must have exactly one of create/read/write/append mode",
        ));
    }
    Ok(binary)
}

/// Fire the PEP 578 `open(path, mode, flags)` event with FileIO's
/// audit shape: the mode keeps only `x`/`r`/`w`/`a`/`+` (FileIO is
/// always binary, so `'b'`/`'t'` never appear), and `path` is the
/// object as passed (an `int` for the fd form).
pub(crate) fn audit_open_event(file: &Object, mode: &str) -> Result<(), RuntimeError> {
    if !crate::trace::any_audit_active() {
        return Ok(());
    }
    let mut audit_mode = String::new();
    for ch in ['x', 'r', 'w', 'a', '+'] {
        if mode.contains(ch) {
            audit_mode.push(ch);
        }
    }
    crate::stdlib::sys::audit_event(
        "open",
        &[file.clone(), Object::from_str(audit_mode), Object::Int(0)],
    )
}

pub(crate) fn b_open(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    use std::fs::OpenOptions;
    if args.is_empty() {
        return Err(type_error("open() missing required argument: 'file'"));
    }
    let mode = match args.get(1) {
        Some(Object::Str(m)) => m.to_string(),
        // `b_open_kw` pads unset positional slots with `None` when a later
        // keyword (e.g. `encoding=`) is supplied, so treat a `None` mode the
        // same as an omitted one (`"r"`), not as a type error.
        None | Some(Object::None) => "r".to_owned(),
        Some(_) => return Err(type_error("open() mode must be str")),
    };
    validate_open_mode(&mode)?;
    // `closefd` (positional slot 6, default True). When False the caller
    // keeps ownership of the descriptor — closing the stream detaches the fd
    // without `close(2)`. Only meaningful for the `open(fd, …)` form; CPython
    // raises `ValueError` for `closefd=False` with a path.
    let closefd = match args.get(6) {
        None | Some(Object::None) => true,
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        Some(_) => true,
    };
    let is_fd = matches!(&args[0], Object::Int(_) | Object::Bool(_));
    // PEP 578 — the `open` event fires from `FileIO.__init__` in
    // CPython, before any syscall (including the fstat of an adopted
    // fd) and before the `closefd` check, with FileIO's own mode
    // string (no 'b'/'t': `open(x, "rb")` audits mode "r").
    audit_open_event(&args[0], &mode)?;
    if !closefd && !is_fd {
        return Err(value_error("Cannot use closefd=False with file name"));
    }
    // `open(fd, …)` adopts an already-open raw file descriptor
    // (produced by `os.open`); the file's `name` is the fd itself.
    #[cfg(unix)]
    if is_fd {
        use std::os::unix::io::FromRawFd;
        let fd_i64 = match &args[0] {
            // CPython 3.12+: a `bool` descriptor warns ("bool is used as a
            // file descriptor") and then behaves as fd 0/1 (test_fileio
            // `testBooleanFd`, run under an escalating warning filter).
            Object::Bool(b) => {
                crate::stdlib::os::warn_bool_as_fd()?;
                i64::from(*b)
            }
            Object::Int(n) => *n,
            _ => unreachable!("is_fd checked above"),
        };
        if fd_i64 < 0 {
            // CPython `_io_FileIO___init___impl`: rejected before any syscall.
            return Err(value_error("negative file descriptor"));
        }
        let fd = i32::try_from(fd_i64)
            .map_err(|_| crate::error::value_error("file descriptor out of range"))?;
        // CPython fstat's the descriptor at construction — *before* adopting
        // it, so a failure never closes the caller's fd: a stale descriptor
        // is `OSError(EBADF)` here (test_fileio `testInvalidFd`) and a
        // directory is `EISDIR` (`testOpenDirFD`, fileio's dircheck).
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        let st = unsafe { st.assume_init() };
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::from_raw_os_error(libc::EISDIR),
            ));
        }
        // SAFETY: ownership of the fd transfers to the new File; it was
        // handed out by os.open (or dup) and is closed exactly once when
        // the PyFile drops — unless `closefd=False`, in which case the
        // PyFile detaches the fd on close instead of running `close(2)`.
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        let file = PyFile::new(fd.to_string(), mode, FileBackend::Disk(f));
        // `st_blksize` is i32 on macOS and i64 on Linux, so the widening
        // conversion is a no-op there (useless_conversion fires per-target).
        #[allow(clippy::useless_conversion)]
        if st.st_blksize > 1 {
            file.blksize.set(i64::from(st.st_blksize));
        }
        file.name_is_fd.set(true);
        file.closefd.set(closefd);
        let binary = file.binary;
        // CPython's `open` *is* `io.open`: validate the text config / buffering
        // and close the adopted descriptor on any rejection rather than leaking
        // it (and emitting a spurious unclosed-file `ResourceWarning`).
        return crate::stdlib::io_full::finish_open(
            Object::File(Rc::new(file)),
            args.get(2),
            args.get(3),
            args.get(4),
            args.get(5),
            binary,
        );
    }
    let (path, name_is_bytes, os_path) = open_path_arg(&args[0])?;
    // A NUL can't cross the C `open(2)` boundary; CPython rejects it up front
    // with `ValueError` (str paths say "character", bytes say "byte") —
    // test_fileio `testConstructorHandlesNULChars`.
    if path.contains('\0') {
        return Err(value_error(if name_is_bytes {
            "embedded null byte"
        } else {
            "embedded null character"
        }));
    }
    let mut opts = OpenOptions::new();
    let mut writing = false;
    for ch in mode.chars() {
        match ch {
            'r' => {
                opts.read(true);
            }
            'w' => {
                opts.write(true).create(true).truncate(true);
                writing = true;
            }
            'a' => {
                opts.write(true).create(true).append(true);
                writing = true;
            }
            'x' => {
                opts.write(true).create_new(true);
                writing = true;
            }
            '+' => {
                opts.read(true).write(true);
            }
            'b' | 't' => {}
            _ => return Err(value_error(format!("invalid mode: '{mode}'"))),
        }
    }
    if !mode.contains('r') && !writing {
        opts.read(true);
    }
    let mut f = opts
        .open(&os_path)
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&path)))?;
    // CPython's `FileIO.__init__` explicitly `lseek`s to the end in append
    // mode "for consistent behaviour" — `open(fn, 'a').tell()` is the file
    // size immediately, before any write (RotatingFileHandler.shouldRollover
    // reads `stream.tell()` right after open — test_logging
    // test_should_rollover).
    if mode.contains('a') {
        use std::io::Seek;
        let _ = f.seek(std::io::SeekFrom::End(0));
    }
    // CPython raises `IsADirectoryError` when `open()` targets a directory
    // (the kernel happily opens a dir fd; the error only surfaces on `read`).
    // Detect it eagerly so `shutil`/`zipfile`/user code see EISDIR at open
    // time, not as a stray "Is a directory" on the first read.
    let meta = f.metadata();
    if meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
        return Err(crate::error::io_error_to_py_named(
            &std::io::Error::from_raw_os_error(21),
            Some(&path),
        ));
    }
    let file = PyFile::new(path, mode, FileBackend::Disk(f));
    // CPython's `FileIO.__init__` captures the filesystem's preferred block
    // size (`_blksize`) from the same fstat, keeping io.DEFAULT_BUFFER_SIZE
    // when the stat has nothing useful (test_fileio `testBlksize`).
    #[cfg(unix)]
    if let Ok(m) = &meta {
        use std::os::unix::fs::MetadataExt;
        if m.blksize() > 1 {
            file.blksize.set(m.blksize() as i64);
        }
    }
    if name_is_bytes {
        file.name_is_bytes.set(true);
    }
    let binary = file.binary;
    // Positional `open(file, mode, buffering, encoding, errors, newline, …)`.
    // Validate the text config / buffering and, on any rejection (illegal
    // `newline`, unbuffered text, unknown encoding/errors), close the
    // freshly-opened file before propagating so we never leak the descriptor or
    // emit a spurious unclosed-file `ResourceWarning` — CPython's `open` *is*
    // `io.open`, which validates before opening.
    crate::stdlib::io_full::finish_open(
        Object::File(Rc::new(file)),
        args.get(2),
        args.get(3),
        args.get(4),
        args.get(5),
        binary,
    )
}

pub(crate) fn b_abs(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "abs")? {
        Object::Int(i) => match i.checked_abs() {
            Some(v) => Ok(Object::Int(v)),
            // i64::MIN.abs() overflows; promote.
            None => Ok(Object::int_from_bigint(num_bigint::BigInt::from(*i).abs())),
        },
        Object::Long(b) => Ok(Object::int_from_bigint(b.abs())),
        // `abs(nan)` allocates in CPython — fresh identity (`abs(x) is x`
        // is False even for a positive NaN).
        Object::Float(f) => Ok(crate::object::fresh_float(f.abs())),
        Object::Complex(c) => {
            // `hypot` (CPython's `_Py_c_abs`) avoids the spurious overflow
            // of `sqrt(re²+im²)`; a non-finite result from finite parts is
            // a genuine magnitude overflow → OverflowError, matching
            // CPython's `complex___abs___impl`.
            let m = c.real.hypot(c.imag);
            if m.is_infinite() && c.real.is_finite() && c.imag.is_finite() {
                return Err(crate::error::overflow_error("absolute value too large"));
            }
            Ok(Object::Float(m))
        }
        Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
        other => Err(type_error(format!(
            "bad operand type for abs(): '{}'",
            other.type_name()
        ))),
    }
}

fn min_or_max(args: &[Object], is_min: bool) -> Result<Object, RuntimeError> {
    let pool: Vec<Object> = if args.len() == 1 {
        let mut out = Vec::new();
        let mut it = args[0].make_iter()?;
        while let Some(v) = it.next_value() {
            out.push(v);
        }
        out
    } else {
        args.to_vec()
    };
    if pool.is_empty() {
        return Err(value_error("min/max arg is an empty sequence"));
    }
    let mut best = pool[0].clone();
    for v in pool.into_iter().skip(1) {
        let ord = v.cmp(&best)?;
        if (is_min && ord.is_lt()) || (!is_min && ord.is_gt()) {
            best = v;
        }
    }
    Ok(best)
}

fn b_min(args: &[Object]) -> Result<Object, RuntimeError> {
    min_or_max(args, true)
}

fn b_max(args: &[Object]) -> Result<Object, RuntimeError> {
    min_or_max(args, false)
}

fn b_sum(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "sum")?;
    let mut total = Object::Int(0);
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        total = crate::binary_op(&total, &v, weavepy_compiler::BinOpKind::Add)?;
    }
    Ok(total)
}

fn b_sorted(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "sorted")?;
    let mut it = iterable.make_iter()?;
    let mut buf: Vec<Object> = Vec::new();
    while let Some(v) = it.next_value() {
        buf.push(v);
    }
    let mut err: Option<RuntimeError> = None;
    buf.sort_by(|a: &Object, b: &Object| match a.cmp(b) {
        Ok(o) => o,
        Err(e) => {
            err = Some(e);
            std::cmp::Ordering::Equal
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(Object::new_list(buf))
}

fn b_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "reversed expected 1 argument, got {}",
            args.len()
        )));
    }
    let iterable = one(args, "reversed")?;
    // `range.__reversed__` returns a *range iterator* — the same type
    // `iter(range(...))` yields (CPython `range_reverse`;
    // test_enumerate.test_range_optimization compares the two types).
    if let Object::Range(r) = iterable {
        if r.big.is_some() {
            let (start, _, step) = r.bounds();
            let len = crate::object::range_len_bigint(r);
            let zero = BigInt::from(0);
            let current = &start + (&len - BigInt::from(1)).max(zero.clone()) * &step;
            let stop = &start - &step;
            return Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::RangeBig {
                current: Box::new(if len > zero { current } else { stop.clone() }),
                stop: Box::new(stop),
                step: Box::new(-step),
            }))));
        }
        let len = crate::object::range_len_i128(r);
        let current = r.start + (len - 1).max(0) * r.step;
        let stop = r.start - r.step;
        return Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::RangeHuge {
            current: if len > 0 { current } else { stop },
            stop,
            step: -r.step,
        }))));
    }
    // A plain list shares its backing store with the reverse-iterator
    // (CPython `list___reversed__`): the iterator holds the *live* list and
    // a descending cursor, so co-pickling `(reversed(xs), xs)` memoizes one
    // list and the iterator sees later mutations (test_list
    // `test_reversed_pickle`).
    if let Object::List(items) = iterable {
        let index = items.borrow().len() as i64 - 1;
        return Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Reversed {
            items: items.clone(),
            index,
            owner: None,
        }))));
    }
    // A list *subclass* instance without `__reversed__`: iterate the
    // native payload live (like the plain-list case above) and pin the
    // instance so its `__del__` can't fire while the iterator is live
    // (test_list test_free_after_iterating via seq_tests).
    if let Object::Instance(inst) = iterable {
        if let Some(Object::List(items)) = inst.native.get() {
            let index = items.borrow().len() as i64 - 1;
            return Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Reversed {
                items: items.clone(),
                index,
                owner: Some(Box::new(iterable.clone())),
            }))));
        }
    }
    // Otherwise materialize the source in *forward* order; the Reversed
    // iterator walks it back-to-front. (CPython's `reversed` uses
    // `__reversed__` or `__len__`+`__getitem__`; a forward snapshot
    // reproduces the same sequence for the finite iterables handled here.)
    let mut it = iterable.make_iter()?;
    let mut buf = Vec::new();
    while let Some(v) = it.next_value() {
        buf.push(v);
    }
    let index = buf.len() as i64 - 1;
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Reversed {
        items: Rc::new(RefCell::new(buf)),
        index,
        owner: None,
    }))))
}

fn b_enumerate_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // `enumerate(iterable, start=0)` — both parameters are documented
    // keywords (CPython argument clinic). Fold keywords onto the
    // positional layout `b_enumerate` consumes.
    let mut iterable: Option<Object> = args.first().cloned();
    let mut start: Option<Object> = args.get(1).cloned();
    if args.len() > 2 {
        return Err(type_error(format!(
            "enumerate() takes at most 2 arguments ({} given)",
            args.len()
        )));
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "iterable" => {
                if iterable.is_some() {
                    return Err(type_error(
                        "argument for enumerate() given by name ('iterable') and position (1)",
                    ));
                }
                iterable = Some(v.clone());
            }
            "start" => {
                if start.is_some() {
                    return Err(type_error(
                        "argument for enumerate() given by name ('start') and position (2)",
                    ));
                }
                start = Some(v.clone());
            }
            other => {
                return Err(type_error(format!(
                    "enumerate() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let Some(iterable) = iterable else {
        return Err(type_error(
            "enumerate() missing required argument 'iterable' (pos 1)",
        ));
    };
    let mut ctor_args = vec![iterable];
    if let Some(s) = start {
        ctor_args.push(s);
    }
    b_enumerate(&ctor_args)
}

fn b_enumerate(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 2 {
        return Err(type_error(format!(
            "enumerate() takes at most 2 arguments ({} given)",
            args.len()
        )));
    }
    let iterable = one(args, "enumerate")?;
    // `start` goes through `PyNumber_Index` (CPython `enum_new`) and may
    // exceed the machine-int range (`enumerate(x, sys.maxsize + 1)` —
    // test_enumerate.TestLongStart).
    let (start, start_big) = if args.len() >= 2 {
        match coerce_index_object(&args[1])? {
            Object::Int(i) => (i, None),
            Object::Long(b) => match i64::try_from(&*b) {
                Ok(i) => (i, None),
                Err(_) => (0, Some(Box::new((*b).clone()))),
            },
            _ => unreachable!("coerce_index_object returns Int or Long"),
        }
    } else {
        (0, None)
    };
    // CPython's `enumerate(x)` wraps `iter(x)` lazily. When `x` is already an
    // iterator, `iter(x)` returns `x` itself, so consuming the enumerate must
    // advance the *same* iterator (test_operator's `indexOf` relies on the
    // source iterator being left at the position after the match). Share the
    // handle for `Object::Iter`; otherwise build a fresh underlying iterator.
    let inner = match iterable {
        Object::Iter(rc) => rc.clone(),
        other => Rc::new(RefCell::new(other.make_iter()?)),
    };
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Enumerate {
        inner,
        count: start,
        count_big: start_big,
    }))))
}

fn b_zip(args: &[Object]) -> Result<Object, RuntimeError> {
    // `zip()` with no iterables is an empty iterator — CPython yields
    // nothing (`list(zip()) == []`). Without this guard the loop below
    // never reaches an exhausted iterator and spins forever appending
    // empty tuples.
    if args.is_empty() {
        return Ok(Object::new_list(Vec::new()));
    }
    let mut iters: Vec<PyIterator> = args
        .iter()
        .map(|a| a.make_iter())
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    loop {
        let mut tup = Vec::with_capacity(iters.len());
        for it in iters.iter_mut() {
            match it.next_value() {
                Some(v) => tup.push(v),
                None => return Ok(Object::new_list(out)),
            }
        }
        out.push(Object::new_tuple(tup));
    }
}

fn b_map(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error(
        "map() requires call-into-interpreter support; use a list comprehension instead",
    ))
}

fn b_filter(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error(
        "filter() requires call-into-interpreter support; use a list comprehension instead",
    ))
}

fn b_all(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "all")?;
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        if !v.is_truthy() {
            return Ok(Object::Bool(false));
        }
    }
    Ok(Object::Bool(true))
}

fn b_any(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "any")?;
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        if v.is_truthy() {
            return Ok(Object::Bool(true));
        }
    }
    Ok(Object::Bool(false))
}

fn b_isinstance(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("isinstance expected 2 arguments"));
    }
    let obj = &args[0];
    let class = &args[1];
    Ok(Object::Bool(matches_classinfo(obj, class)?))
}

pub(crate) fn b_super(args: &[Object]) -> Result<Object, RuntimeError> {
    // `super(C, self)` returns a proxy instance whose class is the
    // real `super` type. Zero-arg form is handled by the VM's
    // call path (it materialises `__class__` and `self` first).
    if !matches!(args.len(), 1 | 2) {
        return Err(type_error(format!(
            "super expected at most 2 arguments, got {}",
            args.len()
        )));
    }
    let class = match &args[0] {
        Object::Type(t) => t.clone(),
        _ => return Err(type_error("super() argument 1 must be a type")),
    };
    if args.len() == 1 {
        return Ok(make_unbound_super(class));
    }
    let receiver = args[1].clone();
    make_super_checked(builtin_types().super_.clone(), class, receiver)
}

/// The one-argument form `super(C)`: an *unbound* super object. It does
/// no MRO walking itself; it is a non-data descriptor whose `__get__`
/// re-binds to `super(C, obj)` (the classic pre-PEP 3135
/// `C._C__super = super(C); self.__super.meth()` idiom). It is an ordinary
/// `super` instance with a `None` receiver.
pub(crate) fn make_unbound_super(class: Rc<crate::types::TypeObject>) -> Object {
    let inst = crate::types::PyInstance {
        class: RefCell::new(builtin_types().super_.clone()),
        dict: Rc::new(RefCell::new({
            let mut d = DictData::default();
            d.insert(DictKey(Object::from_static("__self__")), Object::None);
            d.insert(
                DictKey(Object::from_static("__thisclass__")),
                Object::Type(class),
            );
            d
        })),
        native: std::sync::OnceLock::new(),
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
        c_body: crate::types::CBody::default(),
    };
    Object::Instance(Rc::new(inst))
}

/// CPython `supercheck`: validate the second `super()` argument against the
/// first and return the class whose MRO the proxy walks (`su->obj_type`).
///   * `obj` is `class` itself / a       → class-bound form, walk `obj`'s MRO.
///     subclass of `class`
///   * `type(obj)` is a subclass          → instance / metaclass form, walk
///     of `class`                           `type(obj)`'s MRO.
///   * otherwise                          → TypeError (the interpreter-level
///     [`Interpreter::supercheck_full`] additionally honours `obj.__class__`).
pub(crate) fn supercheck(
    class: &Rc<crate::types::TypeObject>,
    receiver: &Object,
) -> Result<Rc<crate::types::TypeObject>, RuntimeError> {
    if let Object::Type(t) = receiver {
        // A type is trivially a subtype of itself even mid-construction,
        // when its MRO isn't populated yet — CPython's `PyType_IsSubtype`
        // falls back to the `tp_base` chain and short-circuits `a == b`
        // (test_incomplete_super: `super(cls, cls)` inside `mro()`).
        if Rc::ptr_eq(t, class) || t.is_subclass_of(class) {
            return Ok(t.clone());
        }
    }
    let oc = class_of(receiver);
    if oc.is_subclass_of(class) {
        return Ok(oc);
    }
    // CPython `supercheck` names both sides in the failure
    // (test_super.test_supercheck_fail matches the exact shape).
    let (kind, obj_name) = match receiver {
        Object::Type(t) => ("type", t.name.clone()),
        _ => ("instance of", oc.name.clone()),
    };
    Err(type_error(format!(
        "super(type, obj): obj ({kind} {obj_name}) is not an instance or subtype of type ({}).",
        class.name
    )))
}

/// Build a `super` proxy of concrete type `proxy_type` (`super` or a user
/// subclass), bound to `receiver`, walking the MRO after `class` starting
/// from `receiver_class` (`su->obj_type`).
pub(crate) fn build_super_proxy(
    proxy_type: Rc<crate::types::TypeObject>,
    class: Rc<crate::types::TypeObject>,
    receiver: Object,
    receiver_class: Rc<crate::types::TypeObject>,
) -> Object {
    let inst = crate::types::PyInstance {
        class: RefCell::new(proxy_type),
        dict: Rc::new(RefCell::new({
            let mut d = DictData::default();
            d.insert(DictKey(Object::from_static("__self__")), receiver);
            d.insert(
                DictKey(Object::from_static("__thisclass__")),
                Object::Type(class),
            );
            // CPython's `su->obj_type` — the class whose MRO is walked,
            // passed as `owner` to descriptor `__get__`s. Also used to
            // detect the class-bound form (`su->obj == starttype`),
            // where descriptors get a NULL instance (so plain functions
            // come back *unbound*: `super().__new__(cls, v)` must not
            // prepend a second `cls`).
            d.insert(
                DictKey(Object::from_static("__self_class__")),
                Object::Type(receiver_class),
            );
            d
        })),
        native: std::sync::OnceLock::new(),
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
        hash_cache: crate::sync::Cell::new(None),
        finalize_ran: crate::sync::Cell::new(false),
        c_body: crate::types::CBody::default(),
    };
    Object::Instance(Rc::new(inst))
}

/// Build a `super` proxy after validating `receiver` against `class` with
/// the *basic* (no-`__class__`-fallback) [`supercheck`].
pub(crate) fn make_super_checked(
    proxy_type: Rc<crate::types::TypeObject>,
    class: Rc<crate::types::TypeObject>,
    receiver: Object,
) -> Result<Object, RuntimeError> {
    let receiver_class = supercheck(&class, &receiver)?;
    Ok(build_super_proxy(
        proxy_type,
        class,
        receiver,
        receiver_class,
    ))
}

/// `super.__init__(self, type[, obj])` — populates a freshly allocated
/// proxy (used when `class mysuper(super)` is instantiated and its
/// `__init__` chains to `super().__init__(...)`). For the no-arg /
/// `super(C, x)` builtin paths the proxy is built directly by
/// [`make_super_checked`]; here we fill an already-created instance.
pub fn super_init_impl(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("super.__init__ requires a super instance")),
    };
    let class = match args.get(1) {
        Some(Object::Type(t)) => t.clone(),
        None => return Ok(Object::None),
        Some(_) => return Err(type_error("super() argument 1 must be a type")),
    };
    let receiver = args.get(2).cloned().unwrap_or(Object::None);
    let mut d = target.dict.borrow_mut();
    d.insert(
        DictKey(Object::from_static("__thisclass__")),
        Object::Type(class.clone()),
    );
    if matches!(receiver, Object::None) {
        // Unbound `super(C)` — no receiver yet; `__get__` rebinds later.
        d.insert(DictKey(Object::from_static("__self__")), Object::None);
        return Ok(Object::None);
    }
    let receiver_class = supercheck(&class, &receiver)?;
    d.insert(DictKey(Object::from_static("__self__")), receiver);
    d.insert(
        DictKey(Object::from_static("__self_class__")),
        Object::Type(receiver_class),
    );
    Ok(Object::None)
}

/// `super.__get__(self, obj, objtype=None)` — an unbound `super(C)` is a
/// non-data descriptor that rebinds to `super(C, obj)` on access; an
/// already-bound proxy returns itself.
pub fn super_descr_get_impl(args: &[Object]) -> Result<Object, RuntimeError> {
    let this = args.first().cloned().unwrap_or(Object::None);
    let obj = args.get(1).cloned().unwrap_or(Object::None);
    // Already bound (has a non-None __self__) → return self unchanged.
    if let Object::Instance(i) = &this {
        let (bound, class) = {
            let d = i.dict.borrow();
            let bound = d
                .get(&DictKey(Object::from_static("__self__")))
                .map(|v| !matches!(v, Object::None))
                .unwrap_or(false);
            let class = match d.get(&DictKey(Object::from_static("__thisclass__"))) {
                Some(Object::Type(t)) => Some(t.clone()),
                _ => None,
            };
            (bound, class)
        };
        if bound || matches!(obj, Object::None) {
            return Ok(this);
        }
        let Some(class) = class else {
            return Ok(this);
        };
        let proxy_type = i.cls();
        return make_super_checked(proxy_type, class, obj);
    }
    Ok(this)
}

fn b_issubclass(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("issubclass expected 2 arguments"));
    }
    let cls = match &args[0] {
        Object::Type(t) => t.clone(),
        _ => return Err(type_error("issubclass() arg 1 must be a class")),
    };
    let info = &args[1];
    Ok(Object::Bool(class_matches_classinfo_named(
        &cls,
        info,
        "issubclass",
    )?))
}

/// Walk `cls`'s MRO against a single type or tuple of types.
pub fn class_matches_classinfo(
    cls: &crate::types::TypeObject,
    info: &Object,
) -> Result<bool, RuntimeError> {
    class_matches_classinfo_named(cls, info, "isinstance")
}

/// As [`class_matches_classinfo`], with the caller's function name
/// (`isinstance`/`issubclass`) threaded through for CPython-exact
/// error messages.
pub fn class_matches_classinfo_named(
    cls: &crate::types::TypeObject,
    info: &Object,
    func: &str,
) -> Result<bool, RuntimeError> {
    // PEP 604 union (`int | str`) — succeed if any union arm matches.
    // A *parameterized* arm (`list[int] | int`) is not runtime-
    // checkable: CPython's `union_instancecheck` raises TypeError.
    if let Some(args) = crate::is_pep604_union(info) {
        for arg in &args {
            if generic_alias_origin(arg).is_some() {
                return Err(type_error(format!(
                    "{func}() argument 2 cannot contain a parameterized generic"
                )));
            }
        }
        for arg in &args {
            if class_matches_classinfo_named(cls, arg, func)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    // Unwrap PEP 585 generic aliases (`list[int]` → `list`) — CPython
    // treats `isinstance(x, list[int])` as `isinstance(x, list)`.
    if let Some(origin) = generic_alias_origin(info) {
        return class_matches_classinfo(cls, &origin);
    }
    match info {
        Object::Type(t) => Ok(type_subclass_match(cls, t)),
        // `None` inside a union means `type(None)` — match by class
        // name. The `NoneType` class is the unique class with that
        // name (we don't allow user code to redefine it).
        Object::None => Ok(cls.name == "NoneType"),
        Object::Tuple(items) => {
            for it in items.iter() {
                if let Some(args) = crate::is_pep604_union(it) {
                    for arg in &args {
                        if class_matches_classinfo(cls, arg)? {
                            return Ok(true);
                        }
                    }
                } else if let Some(origin) = generic_alias_origin(it) {
                    if class_matches_classinfo(cls, &origin)? {
                        return Ok(true);
                    }
                } else if let Object::Type(t) = it {
                    if type_subclass_match(cls, t) {
                        return Ok(true);
                    }
                } else if matches!(it, Object::None) && cls.name == "NoneType" {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(type_error(
            "issubclass() arg 2 must be a class or tuple of classes",
        )),
    }
}

/// `issubclass(cls, t)` for a single class target, honouring the one
/// structural ABC we expose natively: `os.PathLike`. CPython's
/// `PathLike.__subclasshook__` returns `True` for *any* class that has a
/// non-`None` `__fspath__` in its MRO — even without explicit subclassing
/// (the `FakePath` in `test_os`). Crucially this only fires when the target
/// is *exactly* `os.PathLike`: a user subclass `class A(os.PathLike)` inherits
/// the hook, which returns `NotImplemented` for `cls is not PathLike`, so it
/// falls back to a normal MRO check (`test_pathlike_subclasshook`).
fn type_subclass_match(cls: &crate::types::TypeObject, t: &Rc<crate::types::TypeObject>) -> bool {
    if Rc::ptr_eq(t, &crate::stdlib::os::path_like_type()) {
        return cls
            .lookup("__fspath__")
            .is_some_and(|m| !matches!(m, Object::None));
    }
    cls.is_subclass_of(t)
}

/// Return the `__origin__` of a PEP 585 generic alias (or PEP 604
/// union) wrapped as a `SimpleNamespace`. Returns `None` if `info`
/// isn't a generic alias.
fn generic_alias_origin(info: &Object) -> Option<Object> {
    match info {
        Object::SimpleNamespace(d) => d
            .borrow()
            .get(&crate::object::DictKey(Object::from_static("__origin__")))
            .cloned(),
        _ => None,
    }
}

/// Map any runtime value to the [`crate::types::TypeObject`] that
/// Dunder-named C methods that are *plain methods*, not slot wrappers,
/// in CPython — bound they type as `builtin_function_or_method`, not
/// `method-wrapper`. Two flavors: `__class_getitem__` (a classmethod on
/// every type carrying it) and the `METH_COEXIST` table entries
/// (`set.__contains__` &co. — test_pydoc
/// test_bound_builtin_method_coexist_o).
fn coexist_builtin_dunder(bm: &crate::object::BoundMethod, name: &str) -> bool {
    if name == "__class_getitem__" {
        return true;
    }
    let recv_class = match &bm.receiver {
        Object::Type(t) => t.name.clone(),
        other => class_of(other).name.clone(),
    };
    matches!(
        (recv_class.as_str(), name),
        ("set" | "frozenset", "__contains__")
            | ("dict", "__contains__" | "__getitem__" | "__sizeof__")
            | ("list", "__getitem__")
    )
}

/// `type(x)` would return. Used by `isinstance`/`type()` and a few
/// other reflective code paths. The mapping is the canonical
/// equivalent of CPython's `Py_TYPE(o)`.
pub fn class_of(obj: &Object) -> crate::sync::Rc<crate::types::TypeObject> {
    let bt = builtin_types();
    match obj {
        Object::Instance(inst) => inst.cls(),
        Object::None => bt.none_type.clone(),
        // Unbound never escapes to Python; map it like None defensively.
        Object::Unbound => bt.none_type.clone(),
        Object::Bool(_) => bt.bool_.clone(),
        Object::Int(_) => bt.int_.clone(),
        Object::Long(_) => bt.int_.clone(),
        Object::Float(_) => bt.float_.clone(),
        Object::Complex(_) => bt.complex_.clone(),
        Object::Str(_) => bt.str_.clone(),
        // A surrogate-bearing string is a `str`.
        Object::WStr(_) => bt.str_.clone(),
        Object::Tuple(_) => bt.tuple_.clone(),
        Object::List(_) => bt.list_.clone(),
        Object::Dict(_) => bt.dict_.clone(),
        Object::Range(_) => bt.range_.clone(),
        Object::Slice(_) => bt.slice_.clone(),
        Object::MemoryView(_) => bt.memoryview_.clone(),
        Object::MappingProxy(_) | Object::MappingProxyObj(_) => bt.mappingproxy_.clone(),
        Object::DictView(v) => match v.kind {
            crate::object::DictViewKind::Keys => bt.dict_keys_.clone(),
            crate::object::DictViewKind::Values => bt.dict_values_.clone(),
            crate::object::DictViewKind::Items => bt.dict_items_.clone(),
        },
        // Namespace-shaped objects double as the PEP 585/604 runtime
        // forms; their *class* must report `types.GenericAlias` /
        // `types.UnionType` (CPython: `type(list[int])`, `type(int|str)`).
        Object::SimpleNamespace(d) => {
            let dict = d.borrow();
            if dict
                .get(&DictKey(Object::from_static("__is_pep604_union__")))
                .is_some()
            {
                bt.union_type_.clone()
            } else if dict
                .get(&DictKey(Object::from_static("__origin__")))
                .is_some()
                && dict
                    .get(&DictKey(Object::from_static("__args__")))
                    .is_some()
            {
                // A GenericAlias *subclass* instance carries its class in
                // the namespace dict (stamped by `GenericAlias.__new__`,
                // mirroring CPython's `ga_new` allocating through `cls`).
                if let Some(Object::Type(cls)) =
                    dict.get(&DictKey(Object::from_static("__class__")))
                {
                    cls.clone()
                } else {
                    bt.generic_alias_.clone()
                }
            } else {
                bt.simple_namespace_.clone()
            }
        }
        Object::Type(t) => t.metaclass_or_type(),
        Object::Function(_) => bt.function_.clone(),
        // Rust-implemented callables are `builtin_function_or_method`,
        // distinct from `function`, exactly as in CPython (`type(len)`).
        // Built-in *type-dict* entries are tagged as method/wrapper
        // descriptors (`type(str.lower)` is `method_descriptor`, not
        // `builtin_function_or_method` — test_descr test_qualname).
        Object::Builtin(_) => {
            crate::descr_registry::descr_type(obj).unwrap_or_else(|| bt.builtin_function_.clone())
        }
        // A bound method is its own type in CPython (`type(o.m)` is `method`),
        // which also makes `types.MethodType(func, obj)` construct one.
        // Distinguish what the method wraps, as CPython does:
        //   * Python function        -> `method`
        //   * builtin slot dunder    -> `method-wrapper` (`x.__add__`)
        //   * other builtin callable -> `builtin_function_or_method`
        //     (`[].append` — bound C methods share the C-function type)
        Object::BoundMethod(bm) => match &bm.function {
            Object::Builtin(b) => {
                let n = b.name.trim_start_matches('.');
                if n.starts_with("__") && n.ends_with("__") && !coexist_builtin_dunder(bm, n) {
                    bt.method_wrapper_.clone()
                } else {
                    bt.builtin_function_.clone()
                }
            }
            _ => bt.method_.clone(),
        },
        // A user `@property` is `property`; the numeric getset/member
        // descriptors materialized into the value-type dicts are tagged
        // (`type(float.real)` is `getset_descriptor`, `type(complex.real)`
        // is `member_descriptor` — test_descr test_qualname).
        Object::Property(_) => {
            crate::descr_registry::descr_type(obj).unwrap_or_else(|| bt.property_.clone())
        }
        Object::StaticMethod(_) => bt.staticmethod_.clone(),
        // A ClassMethod wrapping a *registered* native builtin is CPython's
        // `classmethod_descriptor` (`type(dict.__dict__['fromkeys'])`,
        // C-level `__class_getitem__`); a user `@classmethod` stays
        // `classmethod` (inspect's `_NonUserDefinedCallables` distinction,
        // test_inspect test_signature_on_class [classmethod]).
        Object::ClassMethod(w) => {
            let inner = w.func();
            if matches!(inner, Object::Builtin(_))
                && crate::descr_registry::lookup(&inner).is_some()
            {
                bt.classmethod_descriptor_.clone()
            } else {
                bt.classmethod_.clone()
            }
        }
        Object::Bytes(_) => bt.bytes_.clone(),
        Object::ByteArray(_) => bt.bytearray_.clone(),
        Object::Set(_) => bt.set_.clone(),
        Object::FrozenSet(_) => bt.frozenset_.clone(),
        // `enumerate` / `reversed` objects are instances of their own
        // real types in CPython (`type(enumerate([])) is enumerate`).
        // `try_borrow` keeps this safe if the iterator is mid-advance.
        Object::Iter(it) => match it.try_borrow().as_deref() {
            Ok(crate::object::PyIterator::Enumerate { .. }) => bt.enumerate_.clone(),
            Ok(crate::object::PyIterator::Reversed { .. }) => bt.reversed_.clone(),
            _ => bt.iterator_.clone(),
        },
        // Native itertools adapters share the generic iterator type for
        // now; `type(x).__name__` is "iterator" rather than CPython's
        // "islice" until they get dedicated TypeObjects.
        Object::LazyIter(_) => bt.iterator_.clone(),
        Object::Generator(_) => bt.generator_.clone(),
        Object::Coroutine(_) => bt.coroutine_.clone(),
        Object::AsyncGenerator(_) => bt.async_generator_.clone(),
        // The `asend`/`athrow`/`aclose` awaitables get CPython's dedicated
        // types: `asend` (also backing `__anext__`) is `async_generator_asend`;
        // `athrow`/`aclose` are `async_generator_athrow`. Real types let
        // `_collections_abc` register them as `Coroutine`s, so
        // `asyncio.iscoroutine(agen.aclose())` holds and `create_task`/
        // `ensure_future` accept them (PEP 525 finalization, shutdown).
        Object::AsyncGenAwait(a) => match a.kind {
            crate::object::AgenAwaitKind::Send => bt.async_generator_asend_.clone(),
            crate::object::AgenAwaitKind::Throw | crate::object::AgenAwaitKind::Close => {
                bt.async_generator_athrow_.clone()
            }
        },
        Object::Module(_) => bt.module_.clone(),
        Object::SlotDescriptor(_) => bt.member_descriptor_.clone(),
        Object::Code(_) => bt.code_.clone(),
        Object::Cell(_) => bt.cell_.clone(),
        // A native stream reports the faithful CPython io layer for its
        // `IoKind` (`type(open(p,'rb')) is io.BufferedReader`,
        // `type(io.BytesIO()) is io.BytesIO`). These are the very type objects
        // the `io` module exports (both come from the memoised `IoFamily`), so
        // identity and `isinstance`-via-MRO hold. Attribute access on a file is
        // resolved by its own `Object::File` arm, *not* this class's dict, so
        // reporting a rich type here never changes method resolution.
        Object::File(f) => {
            use crate::object::IoKind;
            let fam = crate::stdlib::io::build_iobase_family();
            match f.io_kind.get() {
                IoKind::Raw => fam.fileio.clone(),
                IoKind::BufferedReader => fam.buffered_reader.clone(),
                IoKind::BufferedWriter => fam.buffered_writer.clone(),
                IoKind::BufferedRandom => fam.buffered_random.clone(),
                IoKind::Text => fam.text_io_wrapper.clone(),
                IoKind::BytesIO => fam.bytes_io.clone(),
                IoKind::StringIO => fam.string_io.clone(),
            }
        }
        Object::Frame(_) => bt.frame_.clone(),
        Object::Traceback(_) => bt.traceback_.clone(),
        // A C-API capsule is an opaque cpyext token with no dedicated
        // VM type; report the base `object` type (it never reaches a
        // Python-level `type()` in practice — capsules flow C -> module
        // dict -> C). See RFC 0045.
        Object::Capsule(_) => bt.object_.clone(),
        // A foreign cpyext object's true type is its (often un-bridged)
        // C `PyTypeObject`. When the extension's type is bridged the
        // foreign `get_type` hook yields an `Object::Type`; otherwise we
        // report the base `object` (RFC 0046, wave 4).
        Object::Foreign(s) => match crate::foreign::get_type(s) {
            Object::Type(t) => t,
            _ => bt.object_.clone(),
        },
    }
}

/// Compare a value's runtime type against a class or tuple of classes.
pub fn matches_classinfo(obj: &Object, info: &Object) -> Result<bool, RuntimeError> {
    let bt = builtin_types();
    let obj_class = class_of(obj);
    let _ = bt;
    // Honour a metaclass-defined `__instancecheck__` (PEP 3119): if
    // `info` is a class whose metaclass overrides `__instancecheck__`,
    // route through it. Otherwise fall back to MRO inclusion.
    if let Object::Type(info_cls) = info {
        // `os.PathLike` is a structural ABC (CPython `PathLike.__subclasshook__`):
        // any object whose type defines `__fspath__` is an instance, even
        // without explicit subclassing (e.g. the test suite's `FakePath`).
        if Rc::ptr_eq(info_cls, &crate::stdlib::os::path_like_type()) {
            return Ok(obj_class.lookup("__fspath__").is_some());
        }
        // Native streams (`Object::File`: `open()`, `io.BytesIO`,
        // `io.StringIO`, the std streams) take the fast path and report their
        // class as `object`, but behaviourally satisfy the `io` ABCs. CPython
        // makes `isinstance(io.BytesIO(), io.BufferedIOBase)` true; mirror that
        // (the `pathlib`/`io` suites assert it).
        if let Object::File(f) = obj {
            if let Some(m) = crate::stdlib::io::file_io_abc_match(f, info_cls) {
                return Ok(m);
            }
        }
        let meta = info_cls.metaclass_or_type();
        if let Some(hook) = meta.lookup("__instancecheck__") {
            // We don't have a Vm handle here, so the dispatch path
            // for `isinstance` with metaclass-custom hooks lives in
            // `Vm::do_isinstance_call` (see `Vm::call` interception).
            // Fall through to the regular path; the VM interception
            // will short-circuit before this is reached for the
            // metaclass case.
            let _ = hook;
        }
    }
    let _ = instance_is_subclass;
    class_matches_classinfo(&obj_class, info)
}

fn b_id(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = one(args, "id")?;
    Ok(Object::Int(object_identity(obj)))
}

/// Return a stable integer identity for `obj`. For heap-allocated
/// objects (lists, dicts, tuples, strings, bytes, instances, etc.)
/// this is the pointer to the underlying `Rc` payload, which
/// guarantees uniqueness while the object is alive. For value
/// objects (`int`, `float`, `bool`, `None`) we mix the value with a
/// per-variant salt — matching CPython's "small ints have stable
/// ids" semantics without trying to intern.
pub(crate) fn object_identity(obj: &Object) -> i64 {
    use crate::object::Object;
    // For DST-backed Rc<T> (`Rc<str>`, `Rc<[u8]>`, `Rc<[Object]>`) we
    // can't `as usize` the fat pointer directly; route through the
    // thin pointer of the underlying byte/data buffer.
    fn rc_str_ptr(s: &Rc<str>) -> i64 {
        s.as_ptr() as usize as i64
    }
    fn rc_bytes_ptr(s: &Rc<[u8]>) -> i64 {
        s.as_ptr() as usize as i64
    }
    fn rc_obj_slice_ptr(s: &Rc<[Object]>) -> i64 {
        s.as_ptr() as usize as i64
    }
    match obj {
        Object::Str(s) => rc_str_ptr(s),
        Object::WStr(cps) => cps.as_ptr() as usize as i64,
        Object::Bytes(b) => rc_bytes_ptr(b),
        Object::ByteArray(b) => Rc::as_ptr(b) as usize as i64,
        Object::List(l) => Rc::as_ptr(l) as usize as i64,
        Object::Tuple(t) => rc_obj_slice_ptr(t),
        Object::Dict(d) => Rc::as_ptr(d) as usize as i64,
        Object::Set(s) => Rc::as_ptr(s) as usize as i64,
        Object::FrozenSet(s) => Rc::as_ptr(s) as usize as i64,
        Object::Function(f) => Rc::as_ptr(f) as usize as i64,
        Object::Builtin(b) => Rc::as_ptr(b) as usize as i64,
        Object::BoundMethod(m) => Rc::as_ptr(m) as usize as i64,
        Object::Instance(i) => Rc::as_ptr(i) as usize as i64,
        Object::Type(t) => Rc::as_ptr(t) as usize as i64,
        Object::Module(m) => Rc::as_ptr(m) as usize as i64,
        Object::Range(r) => Rc::as_ptr(r) as usize as i64,
        Object::Slice(s) => Rc::as_ptr(s) as usize as i64,
        Object::Complex(c) => Rc::as_ptr(c) as usize as i64,
        Object::Long(l) => Rc::as_ptr(l) as usize as i64,
        Object::Generator(g) => Rc::as_ptr(g) as usize as i64,
        Object::Coroutine(g) => Rc::as_ptr(g) as usize as i64,
        Object::AsyncGenerator(g) => Rc::as_ptr(g) as usize as i64,
        Object::AsyncGenAwait(a) => Rc::as_ptr(a) as usize as i64,
        Object::File(f) => Rc::as_ptr(f) as usize as i64,
        Object::Property(p) => Rc::as_ptr(p) as usize as i64,
        Object::StaticMethod(m) => Rc::as_ptr(m) as usize as i64,
        Object::ClassMethod(m) => Rc::as_ptr(m) as usize as i64,
        Object::SlotDescriptor(s) => Rc::as_ptr(s) as usize as i64,
        Object::Frame(f) => Rc::as_ptr(f) as usize as i64,
        Object::Traceback(t) => Rc::as_ptr(t) as usize as i64,
        Object::MemoryView(m) => Rc::as_ptr(m) as usize as i64,
        Object::MappingProxy(p) => Rc::as_ptr(p) as usize as i64,
        Object::MappingProxyObj(o) => Rc::as_ptr(o) as usize as i64,
        Object::DictView(v) => Rc::as_ptr(v) as usize as i64,
        Object::SimpleNamespace(n) => Rc::as_ptr(n) as usize as i64,
        Object::Code(c) => Rc::as_ptr(c) as usize as i64,
        Object::Cell(c) => Rc::as_ptr(c) as usize as i64,
        Object::Iter(i) => Rc::as_ptr(i) as usize as i64,
        Object::LazyIter(l) => Rc::as_ptr(l) as usize as i64,
        Object::Capsule(c) => Rc::as_ptr(c) as usize as i64,
        // `id()` of a foreign proxy is the underlying `PyObject*` — the
        // cpyext identity, consistent with `is`/`eq` (RFC 0046).
        Object::Foreign(s) => s.ptr as i64,
        Object::Int(i) => i.wrapping_mul(0x9E37_79B9_7F4A_7C15u64 as i64),
        Object::Float(f) => (f.to_bits() as i64) ^ 0x0123_4567_89AB_CDEFu64 as i64,
        Object::Bool(b) => {
            if *b {
                0x100
            } else {
                0x101
            }
        }
        Object::None => 0x4E6F_6E65, // 'None' as bytes — stable sentinel.
        Object::Unbound => 0x4E6F_6E66,
    }
}

/// Structural hash for primitives. Mirrors CPython's "hash by value"
/// semantics for the built-in immutable types we support.
/// Reject values that cannot serve as a dict/set key, matching CPython:
/// `list`/`dict`/`set`/`bytearray`/`slice` are unhashable, and a `tuple`
/// is unhashable iff any element is (the hash recurses). `frozenset` is
/// hashable by construction. Instances carry their own `__hash__`/`None`
/// marker handled by the VM's `do_hash_call`, so they pass here.
pub fn ensure_hashable(obj: &Object) -> Result<(), RuntimeError> {
    let name = match obj {
        Object::List(_) => "list",
        Object::Dict(_) => "dict",
        Object::Set(_) => "set",
        Object::ByteArray(_) => "bytearray",
        // Slices are hashable since 3.12 (gh-101335) — like a tuple,
        // hashability recurses into the members.
        Object::Slice(s) => {
            ensure_hashable(&s.start)?;
            ensure_hashable(&s.stop)?;
            ensure_hashable(&s.step)?;
            return Ok(());
        }
        Object::Tuple(items) => {
            for it in items.iter() {
                ensure_hashable(it)?;
            }
            return Ok(());
        }
        // A class whose *metaclass* stores the `__hash__ = None`
        // anti-registration marker is unhashable (`class M(type):
        // __hash__ = None`; `hash(A)` for `A(metaclass=M)` raises).
        Object::Type(t) => {
            let meta = t.metaclass_or_type();
            if !Rc::ptr_eq(&meta, &crate::builtin_types::builtin_types().type_)
                && matches!(meta.lookup("__hash__"), Some(Object::None))
            {
                return Err(type_error(format!("unhashable type: '{}'", meta.name)));
            }
            return Ok(());
        }
        // `memory_hash` gates on view state with ValueErrors, not the
        // generic TypeError (test_memoryview): released views, writable
        // views, and non-byte formats don't hash. A cached hash bypasses
        // the checks — releasing a view keeps its stored hash value.
        Object::MemoryView(mv) => {
            if mv.hash.get() != -1 {
                return Ok(());
            }
            if mv.released.get() {
                return Err(crate::error::value_error(
                    "operation forbidden on released memoryview object",
                ));
            }
            if !mv.readonly.get() {
                return Err(crate::error::value_error(
                    "cannot hash writable memoryview object",
                ));
            }
            if !matches!(mv.format.borrow().as_str(), "B" | "b" | "c") {
                return Err(crate::error::value_error(
                    "memoryview: hashing is restricted to formats 'B', 'b' or 'c'",
                ));
            }
            return Ok(());
        }
        // A PEP 604 union hashes as a frozenset of its args (CPython
        // `union_hash`), so an unhashable member propagates.
        Object::SimpleNamespace(_) => {
            if let Some(args) = crate::is_pep604_union(obj) {
                for a in &args {
                    ensure_hashable(a)?;
                }
            }
            return Ok(());
        }
        // An instance whose class carries the `__hash__ = None`
        // anti-registration marker (explicit, or implicit from defining
        // `__eq__`) is unhashable — `PyObject_Hash` raises before any
        // container lookup runs (test_import's unhashable-`__name__`
        // str subclass in set membership).
        Object::Instance(inst) => {
            if matches!(inst.cls().lookup("__hash__"), Some(Object::None)) {
                return Err(type_error(format!(
                    "unhashable type: '{}'",
                    inst.cls().name
                )));
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    Err(type_error(format!("unhashable type: '{name}'")))
}

/// The `DictKey` used to *insert* `obj` into a set/frozenset: a built-in
/// unhashable container (`list`/`dict`/`set`/`bytearray`/`slice`, or a
/// tuple containing one) raises `TypeError: unhashable type: 'X'` just like
/// CPython. Instances pass through — their `__hash__`/`None` is dispatched
/// lazily by the `DictKey` hasher.
pub(crate) fn set_insert_key(obj: &Object) -> Result<DictKey, RuntimeError> {
    ensure_hashable(obj)?;
    Ok(DictKey(obj.clone()))
}

/// The `DictKey` used for set *membership* tests (`in`, `remove`, `discard`):
/// like [`set_insert_key`], but a `set` operand — itself unhashable — is
/// looked up as the equivalent `frozenset`. This reproduces CPython's
/// `set_lookkey`, which retries an unhashable `set` key as a temporary
/// frozenset so `{1, 2} in {frozenset({1, 2})}` is `True` and
/// `myset.discard({1, 2})` finds a stored `frozenset({1, 2})`.
pub(crate) fn set_membership_key(obj: &Object) -> Result<DictKey, RuntimeError> {
    if let Object::Set(s) = obj {
        let body = s.borrow().clone();
        return Ok(DictKey(Object::FrozenSet(Rc::new(
            crate::object::FrozenSetObj::new(body),
        ))));
    }
    if let Err(e) = ensure_hashable(obj) {
        // CPython `set_lookkey`'s retry also covers set *subclass*
        // instances (PyAnySet_Check): an unhashable set-shaped key is
        // looked up as the equivalent frozenset
        // (test_set.TestSetSubclass.test_contains). A subclass that
        // defines its own `__hash__` never reaches here.
        if let Object::Instance(inst) = obj {
            if let Some(Object::Set(s)) = inst.native.get() {
                let body = s.borrow().clone();
                return Ok(DictKey(Object::FrozenSet(Rc::new(
                    crate::object::FrozenSetObj::new(body),
                ))));
            }
        }
        return Err(e);
    }
    Ok(DictKey(obj.clone()))
}

pub fn hash_object(obj: &Object) -> Result<Object, RuntimeError> {
    ensure_hashable(obj)?;
    // Single source of truth shared with `DictKey`'s hasher: the numeric
    // tower uses CPython's exact reduction modulo 2**61-1 (so equal values of
    // different numeric types hash identically and specials match
    // `sys.hash_info`); `str`/`bytes`/`tuple`/`frozenset` get a stable
    // value hash; an int/str/… subclass hashes as its wrapped value; a custom
    // `__hash__` is dispatched through the interpreter. Everything else hashes
    // by allocation identity. Keeping `hash()` and dict bucketing in lockstep
    // is what makes custom `__hash__`/`__eq__` keys interoperate with built-in
    // values in a `set`/`dict`.
    if let Some(h) = crate::object::py_hash_value(obj) {
        return Ok(Object::Int(h));
    }
    Ok(Object::Int(crate::object::identity_hash(obj)))
}

fn b_hash(args: &[Object]) -> Result<Object, RuntimeError> {
    hash_object(one(args, "hash")?)
}

/// The `co_*` surface of a code object (`code_synthetic_attr`), for
/// `dir()` over `types.CodeType` and code instances.
const CODE_ATTR_NAMES: &[&str] = &[
    "co_argcount",
    "co_cellvars",
    "co_code",
    "co_consts",
    "co_exceptiontable",
    "co_filename",
    "co_firstlineno",
    "co_flags",
    "co_freevars",
    "co_kwonlyargcount",
    "co_lines",
    "co_linetable",
    "co_lnotab",
    "co_name",
    "co_names",
    "co_nlocals",
    "co_positions",
    "co_posonlyargcount",
    "co_qualname",
    "co_stacksize",
    "co_varnames",
    "replace",
    "_varname_from_oparg",
];

/// `dir(obj)` — return a sorted list of names available on *obj*.
/// Mirrors CPython's "best effort" introspection: walk the class
/// MRO, the instance dict, the module dict, or — for built-ins —
/// fall back to a small list of dunder names. We deliberately keep
/// this loose because runtime helpers (typing, dataclasses, abc)
/// only need it to enumerate user attributes.
pub fn b_dir(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();
    let obj = one(args, "dir")?;
    // Set when `getattr(obj, '__class__')` would fail — an *unset*
    // `__class__` slot (`__slots__ = ['__class__', …]`) hides the type
    // from `object.__dir__` entirely (test_builtin test_dir).
    let mut class_hidden = false;
    // CPython's traceback type has its own `tb_dir`: exactly the four
    // `tb_*` getsets, no object dunders (test_builtin test_dir asserts
    // `len(dir(tb)) == 4`).
    if matches!(obj, Object::Traceback(_)) {
        return Ok(Object::new_list(
            ["tb_frame", "tb_lasti", "tb_lineno", "tb_next"]
                .into_iter()
                .map(Object::from_static)
                .collect(),
        ));
    }
    match obj {
        Object::Instance(inst) => {
            for k in inst.dict.borrow().keys() {
                if let Object::Str(s) = &k.0 {
                    names.insert(s.to_string());
                }
            }
            class_hidden = matches!(
                inst.cls().lookup("__class__"),
                Some(Object::SlotDescriptor(_))
            ) && inst.slot_get("__class__").is_none();
            if !class_hidden {
                for t in inst.cls().mro.borrow().iter() {
                    for k in t.dict.borrow().keys() {
                        if let Object::Str(s) = &k.0 {
                            names.insert(s.to_string());
                        }
                    }
                }
            }
        }
        Object::Type(t) => {
            for cls in t.mro.borrow().iter() {
                for k in cls.dict.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
                    }
                }
            }
            // `types.CodeType`'s getsets are synthesized in `load_attr`
            // (`code_synthetic_attr`), not stored in the type dict;
            // `dir(CodeType)` must still list them — `unittest.mock`'s
            // `AsyncMock` builds its fake `__code__` from
            // `NonCallableMock(spec_set=CodeType)` and then sets
            // `co_flags`/`co_argcount` on it.
            if t.name == "code" && t.flags.is_builtin {
                for n in CODE_ATTR_NAMES {
                    names.insert((*n).to_string());
                }
            }
        }
        Object::Module(m) => {
            // CPython sets `__spec__`/`__loader__` eagerly at import;
            // the native importer synthesizes them lazily on first
            // *attribute read*, which `dir()`'s raw dict walk would
            // bypass — so modules natively imported looked spec-less
            // (test_decimal's CheckAttributes diffs `dir(C)` against
            // `dir(P)`, and P is the natively imported `_pydecimal`).
            // Trigger the synthesis first; harmless no-op once done.
            let missing_spec = !m
                .dict
                .borrow()
                .contains_key(&crate::object::DictKey(Object::from_static("__spec__")));
            if missing_spec {
                if let Some(p) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by the enclosing VM dispatch
                    // frame on this thread; the GIL keeps it exclusive.
                    let _ = unsafe { &mut *p }.ensure_module_spec(m);
                }
            }
            for k in m.dict.borrow().keys() {
                if let Object::Str(s) = &k.0 {
                    names.insert(s.to_string());
                }
            }
        }
        Object::Function(f) => {
            // CPython's `function.__dir__` = type attributes ∪ instance
            // `__dict__`. The instance part matters to code that *copies*
            // attributes by enumerating `dir(fn)` — hypothesis's `@given`
            // transplants pytest marks onto its wrapper that way, so a dir
            // that hid `f.pytestmark` silently dropped every
            // `@pytest.mark.parametrize` stacked under `@given`.
            for k in f.attrs().borrow().keys() {
                if let Object::Str(s) = &k.0 {
                    names.insert(s.to_string());
                }
            }
            for n in [
                "__annotations__",
                "__builtins__",
                "__call__",
                "__closure__",
                "__code__",
                "__defaults__",
                "__dict__",
                "__doc__",
                "__get__",
                "__globals__",
                "__kwdefaults__",
                "__module__",
                "__name__",
                "__qualname__",
                "__type_params__",
            ] {
                names.insert(n.to_string());
            }
            for t in class_of(obj).mro.borrow().iter() {
                for k in t.dict.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
                    }
                }
            }
        }
        Object::BoundMethod(bm) => {
            // CPython `method.__dir__`: the method type's surface plus
            // everything on the wrapped function — `method_getattro`
            // forwards unknown reads to `__func__`, so arbitrary metadata
            // set on the function (`f.known_attr = 7`) must appear in
            // `dir(obj.f)` too (test_funcattrs).
            if let Object::List(items) = b_dir(&[bm.function.clone()])? {
                for it in items.borrow().iter() {
                    if let Object::Str(s) = it {
                        names.insert(s.to_string());
                    }
                }
            }
            names.insert("__func__".to_string());
            names.insert("__self__".to_string());
            for t in class_of(obj).mro.borrow().iter() {
                for k in t.dict.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
                    }
                }
            }
        }
        other => {
            // Generic objects: `object.__dir__` ≈ the type's attributes.
            for t in class_of(other).mro.borrow().iter() {
                for k in t.dict.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
                    }
                }
            }
            // A namespace-shaped object (PEP 585 GenericAlias,
            // SimpleNamespace) carries per-object attributes in its dict —
            // CPython's `ga_dir`/`object.__dir__` include them
            // (`'__origin__' in dir(list[int])`).
            if let Object::SimpleNamespace(d) = other {
                for k in d.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
                    }
                }
                // CPython `ga_dir`: a generic alias reports `dir(origin)`
                // plus its own attributes, so `dir(list[int])` is a
                // superset of `dir(list)` (test_genericalias.test_dir).
                let origin = d
                    .borrow()
                    .get(&crate::object::StrKey("__origin__"))
                    .cloned();
                if let Some(Object::Type(t)) = origin {
                    for cls in t.mro.borrow().iter() {
                        for k in cls.dict.borrow().keys() {
                            if let Object::Str(s) = &k.0 {
                                names.insert(s.to_string());
                            }
                        }
                    }
                }
            }
            // The generator family's methods and introspection attrs are
            // synthesized in `load_attr` rather than stored in type
            // dicts; surface the same names CPython's type dicts hold.
            if matches!(other, Object::Code(_)) {
                for n in CODE_ATTR_NAMES {
                    names.insert((*n).to_string());
                }
            }
            let extra: &[&str] = match other {
                // CPython's builtin-function / descriptor types expose
                // these as getsets, invisible to the type-dict walk
                // (`'__name__' in dir(vars(object)['__delattr__'])` —
                // test_inspect test_getmembers_descriptors filters on it).
                Object::Builtin(_) => &[
                    "__name__",
                    "__qualname__",
                    "__text_signature__",
                    "__module__",
                    "__self__",
                ],
                // member_descriptor's getsets (`'__name__' in
                // dir(A.__dict__['__weakref__'])`) — the same
                // test_getmembers_descriptors filter keys on them to
                // discard standard class attributes.
                Object::SlotDescriptor(_) => &["__name__", "__qualname__", "__objclass__"],
                Object::Generator(_) => &[
                    "close",
                    "send",
                    "throw",
                    "gi_code",
                    "gi_frame",
                    "gi_running",
                    "gi_suspended",
                    "gi_yieldfrom",
                    "__next__",
                    "__iter__",
                    "__name__",
                    "__qualname__",
                    "__del__",
                ],
                Object::Coroutine(_) => &[
                    "close",
                    "send",
                    "throw",
                    "cr_await",
                    "cr_code",
                    "cr_frame",
                    "cr_origin",
                    "cr_running",
                    "cr_suspended",
                    "__await__",
                    "__name__",
                    "__qualname__",
                    "__del__",
                ],
                Object::AsyncGenerator(_) => &[
                    "aclose",
                    "asend",
                    "athrow",
                    "ag_await",
                    "ag_code",
                    "ag_frame",
                    "ag_running",
                    "ag_suspended",
                    "__aiter__",
                    "__anext__",
                    "__name__",
                    "__qualname__",
                    "__del__",
                ],
                // `property`'s attributes are resolved in `load_attr`
                // rather than stored in the type dict; surface the same
                // names CPython's `property.__dict__` exposes so
                // `dir(property_instance)` lists them (test_descr
                // test_properties).
                Object::Property(_) => &[
                    "fget",
                    "fset",
                    "fdel",
                    "getter",
                    "setter",
                    "deleter",
                    "__doc__",
                    "__get__",
                    "__set__",
                    "__delete__",
                    "__set_name__",
                    "__isabstractmethod__",
                ],
                _ => &[],
            };
            for n in extra {
                names.insert((*n).to_string());
            }
        }
    }
    // `object`'s C-level surface that WeavePy synthesizes in attribute
    // lookup rather than storing in the type dict. Every non-module
    // `dir()` walks an MRO ending at `object`, so CPython always lists
    // these (test_descrtut tut3 checks `dir(list)` verbatim).
    if !matches!(obj, Object::Module(_)) && !class_hidden {
        for n in [
            "__class__",
            "__dir__",
            "__doc__",
            "__format__",
            "__getstate__",
            "__repr__",
            "__sizeof__",
            "__str__",
        ] {
            names.insert(n.to_string());
        }
    }
    Ok(Object::new_list(
        names.into_iter().map(Object::from_str).collect(),
    ))
}

fn b_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "hex")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0x{:x}", (i.unsigned_abs()))))
            } else {
                Ok(Object::from_str(format!("0x{i:x}")))
            }
        }
        Object::Long(b) => {
            let inner = (**b).clone();
            if inner.is_negative() {
                Ok(Object::from_str(format!("-0x{:x}", -inner)))
            } else {
                Ok(Object::from_str(format!("0x{inner:x}")))
            }
        }
        Object::Bool(b) => Ok(Object::from_str(format!("0x{}", i64::from(*b)))),
        // The `__index__` protocol (CPython `PyNumber_Index`), full-width:
        // a `np.uint64` above `i64::MAX` still formats.
        other => b_hex(&[coerce_index_object(other)?]),
    }
}

fn b_oct(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "oct")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0o{:o}", i.unsigned_abs())))
            } else {
                Ok(Object::from_str(format!("0o{i:o}")))
            }
        }
        Object::Long(b) => {
            let inner = (**b).clone();
            if inner.is_negative() {
                Ok(Object::from_str(format!("-0o{:o}", -inner)))
            } else {
                Ok(Object::from_str(format!("0o{inner:o}")))
            }
        }
        Object::Bool(b) => Ok(Object::from_str(format!("0o{}", i64::from(*b)))),
        // The `__index__` protocol (CPython `PyNumber_Index`).
        other => b_oct(&[coerce_index_object(other)?]),
    }
}

fn b_bin(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "bin")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0b{:b}", i.unsigned_abs())))
            } else {
                Ok(Object::from_str(format!("0b{i:b}")))
            }
        }
        Object::Long(b) => {
            let inner = (**b).clone();
            if inner.is_negative() {
                Ok(Object::from_str(format!("-0b{:b}", -inner)))
            } else {
                Ok(Object::from_str(format!("0b{inner:b}")))
            }
        }
        Object::Bool(b) => Ok(Object::from_str(format!("0b{}", i64::from(*b)))),
        // The `__index__` protocol (CPython `PyNumber_Index`), full-width.
        other => b_bin(&[coerce_index_object(other)?]),
    }
}

fn b_chr(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "chr")? {
        Object::Int(i) => {
            let n = *i;
            if !(0..=0x10_FFFF).contains(&n) {
                return Err(value_error("chr() arg not in range(0x110000)"));
            }
            let n = n as u32;
            // CPython's `chr` accepts lone surrogates (U+D800..U+DFFF) and
            // returns a length-1 `str` holding the surrogate. `char::from_u32`
            // rejects them, so route through the WTF-8 constructor, which yields
            // a `WStr` for the surrogate range and a plain `Str` otherwise.
            Ok(Object::str_from_codepoints(vec![n]))
        }
        // A bignum is out of chr's range by construction (`Long` never
        // demotes when it fits i64): CPython 3.13 reports *ValueError*
        // for any out-of-range int, however wide (test_builtin test_chr,
        // `chr(2**1000)`).
        Object::Long(_) => Err(value_error("chr() arg not in range(0x110000)")),
        // Anything with `__index__` (bool, numpy integer scalars — pandas'
        // merge code does `chr(ord("a") + np.int64(i))`); CPython parses the
        // argument with the `i` converter, which coerces via `__index__`.
        other => match coerce_index_i64(other) {
            Ok(n) => b_chr(&[Object::Int(n)]),
            Err(RuntimeError::PyException(e)) if e.type_name() == "OverflowError" => {
                Err(value_error("chr() arg not in range(0x110000)"))
            }
            Err(e) => Err(e),
        },
    }
}

fn b_ord(args: &[Object]) -> Result<Object, RuntimeError> {
    let arg = one(args, "ord")?;
    let native = arg.native_value();
    match native.as_ref().unwrap_or(arg) {
        Object::Str(s) => {
            let mut chars = s.chars();
            let c = chars.next().ok_or_else(|| {
                type_error("ord() expected a character, but string of length 0 found")
            })?;
            if chars.next().is_some() {
                return Err(type_error(format!(
                    "ord() expected a character, but string of length {} found",
                    s.chars().count()
                )));
            }
            Ok(Object::Int(i64::from(u32::from(c))))
        }
        // A length-1 surrogate-bearing string: `ord(chr(0xD800)) == 0xD800`.
        Object::WStr(cps) => {
            if cps.len() != 1 {
                return Err(type_error(format!(
                    "ord() expected a character, but string of length {} found",
                    cps.len()
                )));
            }
            Ok(Object::Int(i64::from(cps[0])))
        }
        Object::Bytes(b) if b.len() == 1 => Ok(Object::Int(i64::from(b[0]))),
        Object::Bytes(b) => Err(type_error(format!(
            "ord() expected a character, but string of length {} found",
            b.len()
        ))),
        Object::ByteArray(b) => {
            let data = b.borrow();
            if data.len() == 1 {
                Ok(Object::Int(i64::from(data[0])))
            } else {
                Err(type_error(format!(
                    "ord() expected a character, but string of length {} found",
                    data.len()
                )))
            }
        }
        other => Err(type_error(format!(
            "ord() expected string of length 1, but {} found",
            other.type_name()
        ))),
    }
}

/// Placeholder body for `input()`. The real implementation lives in
/// the VM so it can drive `sys.stdin` / `sys.stdout`; the registered
/// builtin carries the `__vm:` prefix so the call-site interception
/// picks it up. See `Vm::do_input_call`.
fn b_input_unsupported(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(runtime_error("input() must be called through the VM"))
}

/// Placeholder for the PEP 695 `__weavepy_*__` intrinsics; the VM
/// intercepts them (they need interpreter state to import `_typing`
/// and mint type parameters), so reaching this body means the
/// dispatcher missed one.
fn b_type_alias_unsupported(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(runtime_error(
        "PEP 695 intrinsics must be called through the VM",
    ))
}

/// `__weavepy_pep604_union__(a, b)` — build the native PEP 604 union
/// object from Python. `_typing.TypeAliasType.__or__` uses this to
/// mirror CPython's `_Py_union_type_or` slot (a `types.UnionType`
/// union, not `typing.Union`).
fn b_pep604_union(args: &[Object]) -> Result<Object, RuntimeError> {
    match args {
        [a, b] => crate::make_pep604_union(a, b),
        _ => Err(type_error(
            "__weavepy_pep604_union__() expects exactly 2 arguments",
        )),
    }
}

/// The VM's named intrinsics (`__weavepy_*__`), resolved on a
/// builtins-dict *miss* in `Interpreter::load_global`. They mirror
/// CPython's `CALL_INTRINSIC_1/2` opcodes: lowering-generated names
/// that must never be observable — `builtins.__dict__` carries no
/// such keys in CPython (test_pickle's `test_builtin_functions`
/// pickles every visible builtin by name and would trip over them).
pub fn vm_intrinsic(name: &str) -> Option<Object> {
    type Table = std::collections::HashMap<&'static str, Object>;
    fn build() -> Table {
        let mut t = Table::new();
        let mut put = |public: &'static str,
                       vm_name: &'static str,
                       call: fn(&[Object]) -> Result<Object, RuntimeError>| {
            t.insert(
                public,
                Object::Builtin(Rc::new(BuiltinFn {
                    name: vm_name,
                    binds_instance: false,
                    call: Box::new(call),
                    call_kw: None,
                })),
            );
        };
        put(
            "__weavepy_set_tp_name__",
            "__weavepy_set_tp_name__",
            b_set_tp_name,
        );
        put(
            "__weavepy_pep604_union__",
            "__weavepy_pep604_union__",
            b_pep604_union,
        );
        // PEP 695 intrinsics (RFC 0051): VM-intercepted via the `__vm:`
        // name prefix (they need interpreter access to import the frozen
        // `_typing` module); see `Interpreter::do_typing_intrinsic`.
        for (public, vm_name) in [
            ("__weavepy_type_alias__", "__vm:type_alias"),
            ("__weavepy_typevar__", "__vm:typevar"),
            ("__weavepy_typevar_with_bound__", "__vm:typevar_with_bound"),
            (
                "__weavepy_typevar_with_constraints__",
                "__vm:typevar_with_constraints",
            ),
            ("__weavepy_paramspec__", "__vm:paramspec"),
            ("__weavepy_typevartuple__", "__vm:typevartuple"),
            ("__weavepy_typeparam_default__", "__vm:typeparam_default"),
            (
                "__weavepy_typeparam_default_starred__",
                "__vm:typeparam_default_starred",
            ),
            ("__weavepy_generic_base__", "__vm:generic_base"),
        ] {
            put(public, vm_name, b_type_alias_unsupported);
        }
        t
    }
    thread_local! {
        static TABLE: Table = build();
    }
    TABLE.with(|t| t.get(name).cloned())
}

/// `pow(base, exp[, mod])` — modular exponentiation when `mod` is
/// given, otherwise `base ** exp`. Mirrors CPython's three-arg
/// `pow` including the negative-exponent + mod case (the modular
/// inverse).
/// Bind a builtin's positional-or-keyword parameters clinic-style:
/// `names` in declaration order, kwargs matched by name, duplicates and
/// unknown names rejected with CPython's messages.
fn bind_named_args(
    fname: &str,
    names: &[&str],
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Vec<Option<Object>>, RuntimeError> {
    if args.len() > names.len() {
        return Err(type_error(format!(
            "{fname}() takes at most {} arguments ({} given)",
            names.len(),
            args.len()
        )));
    }
    let mut slots: Vec<Option<Object>> = vec![None; names.len()];
    for (i, a) in args.iter().enumerate() {
        slots[i] = Some(a.clone());
    }
    for (k, v) in kwargs {
        match names.iter().position(|n| n == k) {
            Some(i) => {
                if slots[i].is_some() {
                    return Err(type_error(format!(
                        "argument for {fname}() given by name ('{k}') and position ({})",
                        i + 1
                    )));
                }
                slots[i] = Some(v.clone());
            }
            None => {
                return Err(type_error(format!(
                    "{fname}() got an unexpected keyword argument '{k}'"
                )))
            }
        }
    }
    Ok(slots)
}

/// `pow(base, exp, mod=None)` with clinic keyword binding (test_builtin
/// test_pow calls `pow(0, exp=0)`).
pub(crate) fn b_pow_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let slots = bind_named_args("pow", &["base", "exp", "mod"], args, kwargs)?;
    let mut bound: Vec<Object> = Vec::new();
    let names = ["base", "exp"];
    for (i, name) in names.iter().enumerate() {
        match &slots[i] {
            Some(v) => bound.push(v.clone()),
            None => {
                return Err(type_error(format!(
                    "pow() missing required argument: '{name}' (pos {})",
                    i + 1
                )))
            }
        }
    }
    if let Some(m) = &slots[2] {
        bound.push(m.clone());
    }
    b_pow(&bound)
}

pub(crate) fn b_pow(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(type_error("pow() takes 2 or 3 arguments"));
    }
    let base = &args[0];
    let exp = &args[1];
    let modulus = args.get(2);
    if let Some(m) = modulus {
        if !matches!(m, Object::None) {
            return pow_modular(base, exp, m);
        }
    }
    pow_simple(base, exp)
}

/// Two-argument `pow(x, y)` — pure functional implementation that
/// covers ints, floats, complex, and bool. Mirrors the
/// integer/float/complex arithmetic the VM's `BinaryOp::Pow` does
/// inline.
/// `float ** float` shared by `pow()` and the `**` operator: a finite
/// negative power of zero is a `ZeroDivisionError`, a fractional power of a
/// negative base yields a `complex` (CPython promotes rather than NaN-ing).
fn float_pow_value(x: f64, y: f64) -> Result<Object, RuntimeError> {
    if x == 0.0 && y < 0.0 && y.is_finite() {
        return Err(crate::error::zero_division_error(
            "0.0 cannot be raised to a negative power",
        ));
    }
    if x < 0.0 && y.fract() != 0.0 && x.is_finite() && y.is_finite() {
        let magnitude = (-x).powf(y);
        let theta = std::f64::consts::PI * y;
        Ok(Object::new_complex(
            magnitude * theta.cos(),
            magnitude * theta.sin(),
        ))
    } else {
        Ok(Object::Float(x.powf(y)))
    }
}

fn pow_simple(base: &Object, exp: &Object) -> Result<Object, RuntimeError> {
    use num_traits::ToPrimitive;
    match (base, exp) {
        (Object::Int(x), Object::Int(y)) => {
            if *y < 0 {
                float_pow_value(*x as f64, *y as f64)
            } else if let Ok(e) = u32::try_from(*y) {
                if let Some(r) = x.checked_pow(e) {
                    Ok(Object::Int(r))
                } else {
                    let big = BigInt::from(*x).pow(e);
                    Ok(Object::int_from_bigint(big))
                }
            } else {
                Err(value_error("pow() exponent too large"))
            }
        }
        (Object::Int(x), Object::Float(y)) => float_pow_value(*x as f64, *y),
        (Object::Float(x), Object::Int(y)) => float_pow_value(*x, *y as f64),
        (Object::Float(x), Object::Float(y)) => float_pow_value(*x, *y),
        (Object::Bool(b), other) => pow_simple(&Object::Int(i64::from(*b)), other),
        (other, Object::Bool(b)) => pow_simple(other, &Object::Int(i64::from(*b))),
        (Object::Long(x), Object::Int(y)) => {
            if *y < 0 {
                let xf = x.to_f64().ok_or_else(|| value_error("int too large"))?;
                float_pow_value(xf, *y as f64)
            } else if let Ok(e) = u32::try_from(*y) {
                Ok(Object::int_from_bigint(x.as_ref().pow(e)))
            } else {
                Err(value_error("pow() exponent too large"))
            }
        }
        (Object::Int(x), Object::Long(y)) => {
            if let Some(e) = y.to_u32() {
                Ok(Object::int_from_bigint(BigInt::from(*x).pow(e)))
            } else {
                Err(value_error("pow() exponent too large"))
            }
        }
        (Object::Long(x), Object::Long(y)) => {
            if let Some(e) = y.to_u32() {
                Ok(Object::int_from_bigint(x.as_ref().pow(e)))
            } else {
                Err(value_error("pow() exponent too large"))
            }
        }
        // CPython's `pow()` is `PyNumber_Power` — the full binary-op
        // protocol, honouring `__pow__`/`__rpow__` and foreign C number
        // slots. Anything with a dispatchable operand (a user instance, a
        // foreign scalar like `np.int64`, or a metaclass operator) routes
        // through the interpreter's `**` dispatch; only when no interpreter
        // is live (or dispatch itself declines) does the canonical
        // TypeError below fire. pandas' decimal extension tests reach this
        // with `pow(2.0, np.int64(...))`.
        (a, b)
            if matches!(
                a,
                Object::Instance(_) | Object::Foreign(_) | Object::Type(_)
            ) || matches!(
                b,
                Object::Instance(_) | Object::Foreign(_) | Object::Type(_)
            ) =>
        {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame still live on
                // this thread; the GIL keeps the access exclusive.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                interp.dispatch_binary_op(a, b, weavepy_compiler::BinOpKind::Pow, &globals)
            } else {
                crate::binary_op(a, b, weavepy_compiler::BinOpKind::Pow)
            }
        }
        _ => Err(type_error(format!(
            "unsupported operand type(s) for pow(): '{}' and '{}'",
            base.type_name(),
            exp.type_name()
        ))),
    }
}

fn pow_modular(base: &Object, exp: &Object, m: &Object) -> Result<Object, RuntimeError> {
    let (b, e, mm) = (
        bigint_from(base, "pow")?,
        bigint_from(exp, "pow")?,
        bigint_from(m, "pow")?,
    );
    if mm.is_zero() {
        return Err(value_error("pow() 3rd argument cannot be 0"));
    }
    use num_bigint::BigInt;
    use num_traits::One;
    // Work modulo |m|; CPython gives the result the *sign* of `m` at the end.
    let m_abs: BigInt = mm.abs();
    // Reduce the base into [0, |m|).
    let mut base_mod: BigInt = ((&b % &m_abs) + &m_abs) % &m_abs;
    let mut exp_val: BigInt = e.clone();
    // A negative exponent means `pow(base, -e, m) == pow(base**-1, e, m)`,
    // where `base**-1` is the modular inverse (CPython 3.8+). The inverse only
    // exists when `gcd(base, m) == 1`; otherwise CPython raises ValueError.
    if e.is_negative() {
        match mod_inverse(&base_mod, &m_abs) {
            Some(inv) => {
                base_mod = inv;
                exp_val = -e;
            }
            None => return Err(value_error("base is not invertible for the given modulus")),
        }
    }
    // Start from `1 % |m|`, not `1`: with modulus 1 every result is 0,
    // including `pow(x, 0, 1)` (CPython `long_pow` reduces the accumulator).
    let mut result: BigInt = BigInt::one() % &m_abs;
    let zero: BigInt = BigInt::from(0i64);
    while exp_val > zero {
        if &exp_val % 2i64 == BigInt::one() {
            result = (&result * &base_mod) % &m_abs;
        }
        exp_val >>= 1;
        base_mod = (&base_mod * &base_mod) % &m_abs;
    }
    // `result` is in [0, |m|); shift into (m, 0] when the modulus is negative
    // so the sign matches CPython's `int.__mod__` convention.
    if mm.is_negative() && !result.is_zero() {
        result += &mm;
    }
    Ok(Object::int_from_bigint(result))
}

/// Modular inverse of `a` (already reduced into `[0, m)`) modulo `m > 0`, via
/// the iterative extended Euclidean algorithm. Returns `None` when `a` and `m`
/// are not coprime (no inverse exists). Result is normalised into `[0, m)`.
fn mod_inverse(a: &num_bigint::BigInt, m: &num_bigint::BigInt) -> Option<num_bigint::BigInt> {
    use num_bigint::BigInt;
    use num_traits::{One, Zero};
    let (mut old_r, mut r) = (a.clone(), m.clone());
    let (mut old_s, mut s) = (BigInt::one(), BigInt::zero());
    while !r.is_zero() {
        let q = &old_r / &r;
        let new_r = &old_r - &q * &r;
        old_r = std::mem::replace(&mut r, new_r);
        let new_s = &old_s - &q * &s;
        old_s = std::mem::replace(&mut s, new_s);
    }
    if !old_r.is_one() {
        return None;
    }
    Some(((old_s % m) + m) % m)
}

fn bigint_from(o: &Object, fn_name: &str) -> Result<BigInt, RuntimeError> {
    match o {
        Object::Int(i) => Ok(BigInt::from(*i)),
        Object::Long(b) => Ok((**b).clone()),
        Object::Bool(b) => Ok(BigInt::from(i64::from(*b))),
        _ => Err(type_error(format!(
            "{fn_name}() requires integer arguments, got '{}'",
            o.type_name()
        ))),
    }
}

/// `breakpoint(*args, **kwargs)` placeholder — the VM intercepts this
/// to honour `sys.breakpointhook` and `PYTHONBREAKPOINT`.
fn b_breakpoint(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(runtime_error("breakpoint() must be called through the VM"))
}

/// `__weavepy_set_tp_name__(cls, name)` — internal hook for stdlib
/// Python modules that re-implement CPython *C static types* in pure
/// Python (`re.Pattern`, `re.Match`). CPython's `tp_name` for those
/// carries the dotted module prefix, and `tp_name`-based error text
/// ("'re.Pattern' object is not callable") prints it; the pure-Python
/// class's bare `__name__` stays untouched.
pub(crate) fn b_set_tp_name(args: &[Object]) -> Result<Object, RuntimeError> {
    match args {
        [Object::Type(t), Object::Str(s)] => {
            t.c_tp_name
                .set(Some(Box::leak(s.to_string().into_boxed_str())));
            Ok(Object::None)
        }
        _ => Err(type_error(
            "__weavepy_set_tp_name__(cls, name) expects a class and a str",
        )),
    }
}

/// `memoryview(obj)` — returns a `MemoryView` over a bytes-like
/// object. We accept `bytes`, `bytearray`, and existing
/// `MemoryView` (which we shallow-copy, matching CPython).
pub fn b_memoryview(args: &[Object]) -> Result<Object, RuntimeError> {
    // Exactly one argument (test_memoryview.test_constructor:
    // `memoryview(ob, ob)` is a TypeError).
    if args.len() > 1 {
        return Err(type_error(format!(
            "memoryview expected 1 argument, got {}",
            args.len()
        )));
    }
    let arg = one(args, "memoryview")?;
    let mv = match arg {
        Object::Bytes(b) => crate::object::PyMemoryView::from_bytes(b.clone()),
        Object::ByteArray(b) => crate::object::PyMemoryView::from_bytearray(b.clone()),
        // A released (or PEP 688 export-restricted) view no longer exports
        // a buffer; CPython's `memory_getbuf` raises before `memory_new`
        // can wrap it.
        Object::MemoryView(mv) if mv.released.get() || mv.restricted.get() => {
            return Err(value_error(
                "operation forbidden on released memoryview object",
            ));
        }
        Object::MemoryView(mv) => mv.shallow_clone(),
        // `mmap.mmap` (and, through it, `multiprocessing` shared-memory
        // arenas) exports the buffer protocol over its raw mapping.
        Object::Instance(inst)
            if inst.cls().name == "mmap"
                && crate::stdlib::mmap_mod::shared_buffer(inst).is_some() =>
        {
            let buf = crate::stdlib::mmap_mod::shared_buffer(inst)
                .expect("shared_buffer present per guard");
            crate::object::PyMemoryView::from_shared(buf)
        }
        // A foreign buffer exporter (numpy's `ndarray`, a Cython `cdef
        // class` with `__getbuffer__`, …). Route through the cpyext
        // bridge, which drives `PyObject_GetBuffer` and returns a
        // faithfully-typed memoryview (`format`/`itemsize`/`shape`).
        Object::Foreign(soul) => return crate::foreign::get_buffer(soul),
        // A faithful C instance that exports the buffer protocol crosses as
        // `Object::Instance` wearing its real type (numpy's `ndarray` is built
        // by its own `tp_new`, not proxied as `Foreign`). Drive its
        // `bf_getbuffer` through the cpyext bridge. A non-exporter instance
        // surfaces the C-side `TypeError`, matching CPython.
        Object::Instance(_) if crate::foreign::is_installed() => {
            return crate::foreign::get_buffer_obj(arg);
        }
        other => {
            return Err(type_error(format!(
                "memoryview: a bytes-like object is required, not '{}'",
                other.type_name_owned()
            )));
        }
    };
    // Record the exporter so `mv.obj` answers with the original object
    // (CPython's `view->obj`). `memoryview(mv)` inherits via shallow_clone.
    if !matches!(arg, Object::MemoryView(_)) {
        mv.exporter.replace(Some(arg.clone()));
    }
    let out = Object::MemoryView(Rc::new(mv));
    if let Object::MemoryView(m) = &out {
        if let Some(exp) = m.exporter.borrow().as_ref() {
            crate::gc_trace::track_memoryview_exporter(&out, exp);
        }
    }
    Ok(out)
}

fn b_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let it = one(args, "next")?;
    let default = args.get(1).cloned();
    if let Object::Iter(it) = it {
        // `next()`'s optional default only suppresses StopIteration; a
        // "changed size during iteration" RuntimeError still propagates.
        match it.borrow_mut().next_value_checked()? {
            Some(v) => Ok(v),
            None => default.ok_or_else(stop_iteration),
        }
    } else if matches!(it, Object::File(_)) {
        // A file *is* its own iterator in CPython (`file.__next__` yields the
        // next line); `next(f)` must drive `__next__` directly, including the
        // `ValueError` on a closed stream (`test_io.test_io_after_close`). The
        // optional default still suppresses the EOF `StopIteration`.
        match file_next(std::slice::from_ref(it)) {
            Ok(v) => Ok(v),
            Err(RuntimeError::PyException(pe))
                if default.is_some() && pe.type_name() == "StopIteration" =>
            {
                Ok(default.unwrap())
            }
            Err(e) => Err(e),
        }
    } else {
        Err(type_error(format!(
            "'{}' object is not an iterator",
            it.type_name()
        )))
    }
}

fn b_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() == 2 {
        // The 2-arg form is handled in [`Vm::do_iter_callable_sentinel`]
        // because it needs VM access to repeatedly invoke the
        // callable. Reaching this builtin path means the caller
        // bypassed the VM dispatch (e.g. via `__call__` on
        // `builtin_iter`); fall back to a stricter error.
        return Err(type_error(
            "iter(callable, sentinel) must be called through the VM",
        ));
    }
    let it = one(args, "iter")?.make_iter()?;
    Ok(Object::Iter(Rc::new(RefCell::new(it))))
}

/// `aiter(async_iterable)` — return its async iterator (PEP 525 builtin,
/// 3.10+). VM-routed through [`crate::Vm::get_aiter`] so `__aiter__`
/// dispatch runs; this fallback only fires if invoked outside the VM.
fn b_aiter(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("aiter() must be called through the VM"))
}

/// Runtime support for `types.coroutine`: return a copy of a generator
/// function whose code carries `CO_ITERABLE_COROUTINE` (CPython sets
/// the flag by replacing `func.__code__`). Generators created by the
/// returned function are accepted by `await` and may `yield from` a
/// coroutine.
fn b_mark_iterable_coroutine(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Function(f)) = args.first() else {
        return Err(type_error(
            "_weavepy_mark_iterable_coroutine() expects a function",
        ));
    };
    let mut code = (*f.code()).clone();
    code.is_iterable_coroutine = true;
    let marked = crate::object::PyFunction {
        name: f.name.clone(),
        code: RefCell::new(Rc::new(code)),
        globals: f.globals.clone(),
        builtins: f.builtins.clone(),
        defaults: f.defaults.clone(),
        kw_defaults: f.kw_defaults.clone(),
        closure: f.closure.clone(),
        // Shared, not copied: `func.__dict__` mutations stay visible on
        // both, matching CPython where the function object is the same.
        attrs: RefCell::new(f.attrs()),
        slots: RefCell::new(f.slots.borrow().clone()),
    };
    Ok(Object::Function(Rc::new(marked)))
}

/// `anext(async_iterator[, default])` — return the awaitable from
/// `__anext__` (3.10+). VM-routed through [`crate::Vm::get_anext`].
fn b_anext(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("anext() must be called through the VM"))
}

pub(crate) fn b_divmod(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("divmod expected 2 arguments"));
    }
    // Float divmod is a single fused operation in CPython with its own
    // ZeroDivisionError message ("float divmod()"), and computing `//`
    // and `%` separately would double-raise with the wrong text.
    fn float_operand(o: &Object) -> Option<Result<f64, RuntimeError>> {
        match o {
            Object::Float(f) => Some(Ok(*f)),
            Object::Int(i) => Some(Ok(*i as f64)),
            Object::Bool(b) => Some(Ok(if *b { 1.0 } else { 0.0 })),
            // An int operand beyond the finite double range raises
            // OverflowError (CPython's PyLong_AsDouble in float divmod);
            // `divmod(1., 1 << 30000)` must not yield ±inf.
            Object::Long(b) => Some(match b.to_f64() {
                Some(f) if f.is_finite() => Ok(f),
                _ => Err(crate::error::overflow_error(
                    "int too large to convert to float",
                )),
            }),
            _ => None,
        }
    }
    if matches!(&args[0], Object::Float(_)) || matches!(&args[1], Object::Float(_)) {
        if let (Some(x), Some(y)) = (float_operand(&args[0]), float_operand(&args[1])) {
            let (x, y) = (x?, y?);
            let (q, r) = crate::py_float_divmod(x, y, "float divmod()")?;
            return Ok(Object::new_tuple(vec![
                crate::object::fresh_float(q),
                crate::object::fresh_float(r),
            ]));
        }
    }
    let q = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::FloorDiv)?;
    let r = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::Mod)?;
    Ok(Object::new_tuple(vec![q, r]))
}

/// `round(number, ndigits=None)` with clinic keyword binding
/// (test_builtin test_round calls `round(number=-8.0, ndigits=-1)`).
pub(crate) fn b_round_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let slots = bind_named_args("round", &["number", "ndigits"], args, kwargs)?;
    let mut bound: Vec<Object> = Vec::new();
    match &slots[0] {
        Some(v) => bound.push(v.clone()),
        None => {
            return Err(type_error(
                "round() missing required argument: 'number' (pos 1)",
            ))
        }
    }
    if let Some(nd) = &slots[1] {
        bound.push(nd.clone());
    }
    b_round(&bound)
}

pub(crate) fn b_round(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = args
        .first()
        .ok_or_else(|| type_error("round() takes at least one argument"))?;
    // `ndigits` must be an integer (or omitted); a `Long` is saturated to
    // `i64` (anything beyond ±323 short-circuits anyway).
    let ndigits = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Int(i)) => Some(*i),
        Some(Object::Bool(b)) => Some(i64::from(*b)),
        Some(Object::Long(b)) => {
            Some(
                b.to_i64()
                    .unwrap_or(if b.is_negative() { i64::MIN } else { i64::MAX }),
            )
        }
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )));
        }
    };
    match value {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => round_int(value, ndigits),
        Object::Float(f) => match ndigits {
            // `round(x)` (no ndigits) rounds to the nearest integer
            // (ties-to-even) and returns an `int`.
            None => {
                if f.is_nan() {
                    return Err(value_error("cannot convert float NaN to integer"));
                }
                if f.is_infinite() {
                    return Err(crate::error::overflow_error(
                        "cannot convert float infinity to integer",
                    ));
                }
                Ok(float_to_int_obj(round_ties_even(*f)))
            }
            // `round(x, n)` returns a `float`, correctly rounded (ties-to-even)
            // to `n` decimal places. Fresh object in CPython (NaN identity).
            Some(n) => double_round(*f, n).map(crate::object::fresh_float),
        },
        _ => Err(type_error("round() argument must be int or float")),
    }
}

/// Round a finite `f64` to the nearest integer, ties to even.
fn round_ties_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r / 2.0).fract() != 0.0 {
        // `x` was a half-integer and `round()` (ties-away) landed on an odd
        // integer; step toward the even neighbour.
        r - x.signum()
    } else {
        r
    }
}

/// Convert an integral `f64` to `int`/`Long`, used by `round(x)`.
fn float_to_int_obj(r: f64) -> Object {
    if (-(9.223_372_036_854_776e18)..9.223_372_036_854_776e18).contains(&r) {
        Object::Int(r as i64)
    } else {
        BigInt::from_f64(r).map_or(Object::Int(0), |b| Object::Long(Rc::new(b)))
    }
}

/// `round(int_like, ndigits)` — non-negative `ndigits` leave the value
/// unchanged; negative `ndigits` round to a power of ten (ties-to-even).
fn round_int(value: &Object, ndigits: Option<i64>) -> Result<Object, RuntimeError> {
    let n = match ndigits {
        None => return Ok(value.clone()),
        Some(n) if n >= 0 => return Ok(value.clone()),
        Some(n) => n,
    };
    // Negative ndigits: round to 10^(-n) via BigInt to stay exact.
    let v = match value {
        Object::Int(i) => BigInt::from(*i),
        Object::Bool(b) => BigInt::from(i64::from(*b)),
        Object::Long(b) => (**b).clone(),
        _ => unreachable!(),
    };
    let pow = (-n) as u32;
    let scale = BigInt::from(10).pow(pow);
    let q = &v / &scale;
    let r = &v - &q * &scale;
    let mut result = q.clone();
    let two = BigInt::from(2);
    // Compare |remainder|*2 to the scale to decide rounding, breaking exact
    // ties toward the even quotient (CPython's round-half-to-even).
    let round_up = match (r.abs() * &two).cmp(&scale) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => (&q % &two) != BigInt::from(0),
    };
    if round_up {
        if v.is_negative() {
            result -= 1;
        } else {
            result += 1;
        }
    }
    let scaled = result * &scale;
    Ok(Object::int_from_bigint(scaled))
}

/// CPython's `double_round`: round `x` to `ndigits` decimal places with
/// round-half-to-even, returning a `float`. Uses round-trip decimal
/// formatting (Rust's formatter rounds ties-to-even, matching dtoa).
fn double_round(x: f64, ndigits: i64) -> Result<f64, RuntimeError> {
    if !x.is_finite() || x == 0.0 {
        return Ok(x);
    }
    // Outside the representable decimal range nothing changes / underflows.
    if ndigits > 323 {
        return Ok(x);
    }
    if ndigits < -308 {
        return Ok(0.0 * x);
    }
    if ndigits >= 0 {
        let s = format!("{:.*}", ndigits as usize, x);
        let r: f64 = s.parse().unwrap_or(x);
        if r.is_infinite() {
            return Err(crate::error::overflow_error(
                "rounded value too large to represent",
            ));
        }
        Ok(r)
    } else {
        let scale = 10f64.powi((-ndigits) as i32);
        let r = round_ties_even(x / scale) * scale;
        if r.is_infinite() {
            return Err(crate::error::overflow_error(
                "rounded value too large to represent",
            ));
        }
        Ok(r)
    }
}

// ---------- str methods ----------

/// Plane-16 PUA window (U+10F800..U+10FFFF, 2048 code points) used to
/// *bridge* lone surrogates (U+D800..U+DFFF) through the `&str`-based string
/// algorithms: a `WStr` receiver/argument is mapped into this window so the
/// existing UTF-8 method bodies run unchanged, then mapped back on output.
///
/// Bridging only activates when a `WStr` actually participates in the call
/// (see [`str_result`]); a plain `str` — even one containing genuine
/// plane-16 PUA characters — takes the untouched fast path. The only
/// (astronomically rare) caveat is mixing a genuine U+10F800..U+10FFFF
/// character into the *same* method call as a lone-surrogate string.
pub(crate) const BRIDGE_BASE: u32 = 0x10_F800;

/// Map a code-point sequence to a Rust `String`, shifting lone surrogates
/// into the [`BRIDGE_BASE`] PUA window. Non-surrogate code points are kept.
pub(crate) fn bridge_encode_cps(cps: &[u32]) -> String {
    let mut s = String::with_capacity(cps.len());
    for &cp in cps {
        let mapped = if (0xD800..=0xDFFF).contains(&cp) {
            BRIDGE_BASE + (cp - 0xD800)
        } else {
            cp
        };
        s.push(char::from_u32(mapped).unwrap_or('\u{FFFD}'));
    }
    s
}

/// Bridge a `str`/`WStr` object to a Rust `String` whose lone surrogates are
/// shifted into the PUA window; `None` for non-string objects. A plain `str`
/// is returned verbatim (it has no surrogates to shift).
pub(crate) fn bridge_str_of(obj: &Object) -> Option<String> {
    match obj {
        Object::Str(s) => Some(s.to_string()),
        Object::WStr(cps) => Some(bridge_encode_cps(cps)),
        _ => None,
    }
}

#[inline]
pub(crate) fn bridge_window(cp: u32) -> bool {
    (BRIDGE_BASE..=BRIDGE_BASE + 0x7FF).contains(&cp)
}

/// Inverse of [`bridge_encode_cps`] over a (possibly bridged) string,
/// canonicalising to a `str` (no surrogates) or `WStr` (some surrogates).
pub(crate) fn bridge_to_object(s: &str) -> Object {
    if !s.chars().any(|ch| bridge_window(ch as u32)) {
        return Object::from_str(s);
    }
    let cps: Vec<u32> = s
        .chars()
        .map(|ch| {
            let cp = ch as u32;
            if bridge_window(cp) {
                0xD800 + (cp - BRIDGE_BASE)
            } else {
                cp
            }
        })
        .collect();
    Object::str_from_codepoints(cps)
}

/// A `str`/`WStr` method receiver as a Rust string: a plain `str` borrows;
/// a `WStr` is bridged (lone surrogates → PUA) into an owned string.
fn str_self(args: &[Object]) -> Result<std::borrow::Cow<'_, str>, RuntimeError> {
    match args.first() {
        Some(Object::Str(s)) => Ok(std::borrow::Cow::Borrowed(s)),
        Some(Object::WStr(cps)) => Ok(std::borrow::Cow::Owned(bridge_encode_cps(cps))),
        _ => Err(type_error("expected str method receiver")),
    }
}

/// A `str`/`WStr` *argument* (not the receiver) as a bridged Rust string;
/// `None` for any non-string object so callers can raise their own
/// method-specific `TypeError`.
pub(crate) fn str_arg_bridged(obj: &Object) -> Option<std::borrow::Cow<'_, str>> {
    match obj {
        Object::Str(s) => Some(std::borrow::Cow::Borrowed(s)),
        Object::WStr(cps) => Some(std::borrow::Cow::Owned(bridge_encode_cps(cps))),
        // A `str` *subclass* instance is accepted anywhere str is —
        // CPython's argument clinic checks `PyUnicode_Check`, which is
        // subtype-inclusive (markupsafe: `super().replace(old, Markup(…))`).
        Object::Instance(inst) => match inst.native.get() {
            Some(native @ (Object::Str(_) | Object::WStr(_))) => str_arg_bridged(native),
            _ => None,
        },
        _ => None,
    }
}

/// Wrap a string-valued method result, mapping bridged surrogates back to a
/// `WStr` *only* when a `WStr` participated in the call (so plain-`str`
/// results — including genuine plane-16 PUA — are never disturbed).
fn str_result(args: &[Object], result: String) -> Object {
    if args.iter().any(|o| matches!(o, Object::WStr(_))) {
        bridge_to_object(&result)
    } else {
        Object::from_str(result)
    }
}

/// CPython parity: enforce positional arity on native `str` methods the
/// way the C argument clinic / `METH_NOARGS` do. `args[0]` is the
/// receiver; `min`/`max` bound the *user-visible* argument count.
fn str_arity(name: &str, args: &[Object], min: usize, max: usize) -> Result<(), RuntimeError> {
    let given = args.len().saturating_sub(1);
    if given > max {
        return Err(type_error(if max == 0 {
            format!("str.{name}() takes no arguments ({given} given)")
        } else {
            format!("{name} expected at most {max} arguments, got {given}")
        }));
    }
    if given < min {
        return Err(type_error(format!(
            "{name} expected at least {min} arguments, got {given}"
        )));
    }
    Ok(())
}

fn str_upper(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("upper", args, 0, 0)?;
    let up = crate::unicode_case::upper(&str_self(args)?);
    Ok(str_result(args, up))
}

fn str_lower(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("lower", args, 0, 0)?;
    let lo = crate::unicode_case::lower(&str_self(args)?);
    Ok(str_result(args, lo))
}

/// `str.casefold()` — aggressive, caseless-matching fold. Distinct from
/// `.lower()`: e.g. `"ß".casefold() == "ss"` and `"ς".casefold() == "σ"`,
/// and folding is context-free (no Greek final-sigma special-casing).
fn str_casefold(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("casefold", args, 0, 0)?;
    let out = crate::unicode_case::casefold(&str_self(args)?);
    Ok(str_result(args, out))
}

fn str_strip(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("strip", args, 0, 1)?;
    let s = str_self(args)?;
    let out = match args.get(1) {
        None | Some(Object::None) => s.trim_matches(crate::unicode_case::is_space).to_owned(),
        Some(arg) => {
            let chars =
                str_arg_bridged(arg).ok_or_else(|| type_error("strip() argument must be str"))?;
            let set: Vec<char> = chars.chars().collect();
            s.trim_matches(|c| set.contains(&c)).to_owned()
        }
    };
    if let Some(same) = str_unchanged_self(args, &s, &out) {
        return Ok(same);
    }
    Ok(str_result(args, out))
}

/// CPython's strip-family identity optimization: `str.strip`/`lstrip`/
/// `rstrip` on an exact `str` return *self* (same object) when nothing
/// was removed. `test_bigmem` asserts `s.lstrip() is s`.
fn str_unchanged_self(args: &[Object], s: &str, out: &str) -> Option<Object> {
    if out.len() == s.len() {
        if let Some(recv @ Object::Str(_)) = args.first() {
            return Some(recv.clone());
        }
    }
    None
}

fn split_maxsplit(o: Option<&Object>) -> Result<i64, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(-1),
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(_) => Err(type_error("maxsplit must be an integer")),
    }
}

/// `str.split` on runs of whitespace (the `sep is None` case), honouring
/// `maxsplit`. Leading/trailing whitespace is stripped and empty fields
/// are dropped, matching CPython (Py_UNICODE_ISSPACE, which covers
/// U+001C..U+001F unlike Rust's `char::is_whitespace`).
fn str_split_whitespace(s: &str, maxsplit: i64) -> Vec<Object> {
    use crate::unicode_case::is_space;
    if maxsplit < 0 {
        return s
            .split(is_space)
            .filter(|f| !f.is_empty())
            .map(Object::from_str)
            .collect();
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut splits = 0;
    while i < n {
        while i < n && is_space(chars[i].1) {
            i += 1;
        }
        if i >= n {
            break;
        }
        if splits >= maxsplit {
            out.push(Object::from_str(s[chars[i].0..].to_string()));
            return out;
        }
        let start = chars[i].0;
        while i < n && !is_space(chars[i].1) {
            i += 1;
        }
        let end = if i < n { chars[i].0 } else { s.len() };
        out.push(Object::from_str(s[start..end].to_string()));
        splits += 1;
    }
    out
}

fn str_split(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    str_arity("split", args, 0, 2)?;
    let s = str_self(args)?;
    let sep = arg_or_kw(args, 1, kwargs, "sep");
    let maxsplit = split_maxsplit(arg_or_kw(args, 2, kwargs, "maxsplit"))?;
    // Bridge field results back to surrogates when a `WStr` was involved
    // (receiver or separator).
    let wrap = |out: Vec<Object>| -> Vec<Object> {
        if args.iter().any(|o| matches!(o, Object::WStr(_))) || matches!(sep, Some(Object::WStr(_)))
        {
            out.into_iter()
                .map(|o| match o {
                    Object::Str(piece) => bridge_to_object(&piece),
                    other => other,
                })
                .collect()
        } else {
            out
        }
    };
    let out: Vec<Object> = match sep {
        None | Some(Object::None) => str_split_whitespace(&s, maxsplit),
        Some(sep_obj) => {
            let sep = str_arg_bridged(sep_obj)
                .ok_or_else(|| type_error("must be str or None, not other"))?;
            if sep.is_empty() {
                return Err(value_error("empty separator"));
            }
            if maxsplit < 0 {
                s.split(&*sep).map(Object::from_str).collect()
            } else {
                s.splitn((maxsplit as usize).saturating_add(1), &*sep)
                    .map(Object::from_str)
                    .collect()
            }
        }
    };
    Ok(Object::new_list(wrap(out)))
}

fn str_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let sep = str_self(args)?.into_owned();
    if args.len() != 2 {
        return Err(type_error("join() expected 1 argument"));
    }
    let mut it = args[1].make_iter()?;
    let mut items = Vec::new();
    while let Some(v) = it.next_value() {
        items.push(v);
    }
    // `PyUnicode_Join` returns a 1-element sequence's item *itself* when it
    // is an exact str, regardless of the separator
    // (test/string_tests.py test_bug1001011 asserts the identity).
    if items.len() == 1 && matches!(&items[0], Object::Str(_) | Object::WStr(_)) {
        return Ok(items[0].clone());
    }
    let mut parts = Vec::new();
    let mut saw_surrogate = matches!(args.first(), Some(Object::WStr(_)));
    for v in &items {
        match v {
            Object::Str(s) => parts.push(s.to_string()),
            Object::WStr(cps) => {
                saw_surrogate = true;
                parts.push(bridge_encode_cps(cps));
            }
            // Accept `str` subclass instances (e.g. email's `ValueTerminal`):
            // CPython's `str.join` treats any `PyUnicode` — subclasses
            // included — as a string item. Unwrap the native payload.
            other => match other.native_value() {
                Some(Object::Str(s)) => parts.push(s.to_string()),
                Some(Object::WStr(cps)) => {
                    saw_surrogate = true;
                    parts.push(bridge_encode_cps(&cps));
                }
                _ => {
                    return Err(type_error(format!(
                        "sequence item: expected str instance, {} found",
                        other.type_name()
                    )))
                }
            },
        }
    }
    let joined = parts.join(&sep);
    Ok(if saw_surrogate {
        bridge_to_object(&joined)
    } else {
        Object::from_str(joined)
    })
}

fn str_startswith(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("startswith", args, 1, 3)?;
    let s = str_self(args)?;
    // PEP 257: ``startswith`` accepts either a string *or* a tuple of strings.
    let target = match args.get(1) {
        Some(obj) => obj,
        None => return Err(type_error("startswith() takes at least 1 argument")),
    };
    let slice = str_apply_start_end(s.as_ref(), args.get(2), args.get(3))?;
    match slice {
        Some(slice) => Ok(Object::Bool(str_match_prefix_suffix(slice, target, true)?)),
        None => Ok(Object::Bool(false)),
    }
}

fn str_endswith(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("endswith", args, 1, 3)?;
    let s = str_self(args)?;
    let target = match args.get(1) {
        Some(obj) => obj,
        None => return Err(type_error("endswith() takes at least 1 argument")),
    };
    let slice = str_apply_start_end(s.as_ref(), args.get(2), args.get(3))?;
    match slice {
        Some(slice) => Ok(Object::Bool(str_match_prefix_suffix(slice, target, false)?)),
        None => Ok(Object::Bool(false)),
    }
}

/// Resolve start/end for `startswith`/`endswith` like CPython's
/// `tailmatch` + `ADJUST_INDICES`: `end` clamps to the length, but
/// `start` is only floored at 0 — a start beyond the (adjusted) end
/// yields `None`, meaning no match *even for an empty needle*
/// (`''.startswith('', 1, 0)` is False — test_userstring).
fn str_apply_start_end<'a>(
    s: &'a str,
    start: Option<&Object>,
    end: Option<&Object>,
) -> Result<Option<&'a str>, RuntimeError> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len() as i64;
    let resolve = |raw: Option<&Object>, default: i64| -> Result<i64, RuntimeError> {
        match raw {
            None | Some(Object::None) => Ok(default),
            Some(Object::Int(i)) => Ok(*i),
            Some(_) => Err(type_error("slice indices must be int or None")),
        }
    };
    let mut start_idx = resolve(start, 0)?;
    let mut end_idx = resolve(end, n)?;
    if start_idx < 0 {
        start_idx = (start_idx + n).max(0);
    }
    if end_idx < 0 {
        end_idx += n;
    }
    let end_idx = end_idx.clamp(0, n);
    if start_idx > end_idx {
        return Ok(None);
    }
    let start_idx = start_idx as usize;
    let end_idx = end_idx as usize;
    let start_byte = chars.get(start_idx).map(|(i, _)| *i).unwrap_or(s.len());
    let end_byte = chars.get(end_idx).map(|(i, _)| *i).unwrap_or(s.len());
    Ok(Some(&s[start_byte..end_byte]))
}

fn str_match_prefix_suffix(
    slice: &str,
    target: &Object,
    prefix: bool,
) -> Result<bool, RuntimeError> {
    let test = |needle: &str| {
        if prefix {
            slice.starts_with(needle)
        } else {
            slice.ends_with(needle)
        }
    };
    match target {
        Object::Str(_) | Object::WStr(_) => {
            let needle = str_arg_bridged(target).expect("str/WStr");
            Ok(test(&needle))
        }
        Object::Tuple(parts) => {
            for item in parts.iter() {
                match str_arg_bridged(item) {
                    Some(needle) => {
                        if test(&needle) {
                            return Ok(true);
                        }
                    }
                    None => {
                        return Err(type_error(
                            "startswith/endswith first arg must be str or tuple of str",
                        ));
                    }
                }
            }
            Ok(false)
        }
        _ => Err(type_error(
            "startswith/endswith first arg must be str or tuple of str",
        )),
    }
}

fn str_replace_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let from = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("replace() expected str")),
    };
    let to = match args.get(2).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("replace() expected str")),
    };
    let (from, to) = (from.as_ref(), to.as_ref());
    let mut count_obj = args.get(3).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "count" => count_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "replace() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let count = match count_obj {
        None | Some(Object::None) => -1i64,
        Some(o) => coerce_index_i64(&o)?,
    };
    if count == 0 {
        return Ok(str_result(args, s.into_owned()));
    }
    let out = if count < 0 {
        s.replace(from, to)
    } else if from.is_empty() {
        // `str::replacen` with an empty pattern matches between every
        // char and at both ends, same as CPython.
        let mut out = String::new();
        let mut done = 0i64;
        for (i, ch) in s.chars().enumerate() {
            let _ = i;
            if done < count {
                out.push_str(to);
                done += 1;
            }
            out.push(ch);
        }
        if done < count {
            out.push_str(to);
        }
        out
    } else {
        s.replacen(from, to, count as usize)
    };
    Ok(str_result(args, out))
}

/// `ADJUST_INDICES`: negative indices offset by length and floored at
/// 0; `end` clamped to length; `start` left unclamped so a start past
/// the end yields an invalid window (`'abc'.find('', 4) == -1`).
fn str_search_window(args: &[Object], total_chars: i64) -> Option<(i64, i64)> {
    let resolve = |arg: Option<&Object>, default: i64| -> i64 {
        match arg {
            None | Some(Object::None) => default,
            Some(o) => match o.as_i64() {
                Some(x) => {
                    if x < 0 {
                        (x + total_chars).max(0)
                    } else {
                        x
                    }
                }
                None => default,
            },
        }
    };
    let start = resolve(args.get(2), 0);
    let end = resolve(args.get(3), total_chars).clamp(0, total_chars);
    if start > end {
        None
    } else {
        Some((start, end))
    }
}

fn str_find(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("find", args, 1, 3)?;
    let s = str_self(args)?;
    let s = s.as_ref();
    let sub = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("find() expected str")),
    };
    let total_chars = str_char_len(s) as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(-1));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    let hay = &s[start_byte..end_byte];
    match hay.find(&*sub) {
        Some(byte_idx) => {
            let abs_byte = byte_idx + start_byte;
            Ok(Object::Int(byte_offset_to_char(s, abs_byte) as i64))
        }
        None => Ok(Object::Int(-1)),
    }
}

/// Whether `s` is pure ASCII, memoized by buffer identity. CPython's PEP 393
/// layout makes char↔byte offset mapping O(1); our UTF-8 `Rc<str>` needs a
/// scan. Callers like `str.find(sub, start)`-in-a-loop (email's
/// `_parseparam`, N=100k windows over one big header) would otherwise turn
/// linear algorithms quadratic. A one-slot cache suffices: hot loops hammer
/// the same haystack repeatedly.
pub(crate) fn str_is_ascii_cached(s: &str) -> bool {
    use std::cell::Cell;
    thread_local! {
        static ASCII_CACHE: Cell<(usize, usize, bool)> = const { Cell::new((0, 0, false)) };
    }
    let key = (s.as_ptr() as usize, s.len());
    ASCII_CACHE.with(|c| {
        let (p, l, v) = c.get();
        if (p, l) == key {
            return v;
        }
        let v = s.is_ascii();
        c.set((key.0, key.1, v));
        v
    })
}

/// Total `len()` in code points, O(1) for (cached-)ASCII strings.
pub(crate) fn str_char_len(s: &str) -> usize {
    if str_is_ascii_cached(s) {
        s.len()
    } else {
        s.chars().count()
    }
}

fn char_offset_to_byte(s: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    if str_is_ascii_cached(s) {
        return n.min(s.len());
    }
    s.char_indices().nth(n).map(|(b, _)| b).unwrap_or(s.len())
}

fn byte_offset_to_char(s: &str, byte: usize) -> usize {
    if str_is_ascii_cached(s) {
        return byte;
    }
    s[..byte].chars().count()
}

fn str_title(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("title", args, 0, 0)?;
    let out = crate::unicode_case::title(&str_self(args)?);
    Ok(str_result(args, out))
}

fn str_capitalize(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("capitalize", args, 0, 0)?;
    let out = crate::unicode_case::capitalize(&str_self(args)?);
    Ok(str_result(args, out))
}

fn str_swapcase(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("swapcase", args, 0, 0)?;
    let out = crate::unicode_case::swapcase(&str_self(args)?);
    Ok(str_result(args, out))
}

fn str_lstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("lstrip", args, 0, 1)?;
    let s = str_self(args)?;
    let out = match args.get(1) {
        None | Some(Object::None) => s
            .trim_start_matches(crate::unicode_case::is_space)
            .to_owned(),
        Some(arg) => {
            let chars =
                str_arg_bridged(arg).ok_or_else(|| type_error("lstrip() argument must be str"))?;
            let set: Vec<char> = chars.chars().collect();
            s.trim_start_matches(|c| set.contains(&c)).to_owned()
        }
    };
    if let Some(same) = str_unchanged_self(args, &s, &out) {
        return Ok(same);
    }
    Ok(str_result(args, out))
}

fn str_rstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("rstrip", args, 0, 1)?;
    let s = str_self(args)?;
    let out = match args.get(1) {
        None | Some(Object::None) => s.trim_end_matches(crate::unicode_case::is_space).to_owned(),
        Some(arg) => {
            let chars =
                str_arg_bridged(arg).ok_or_else(|| type_error("rstrip() argument must be str"))?;
            let set: Vec<char> = chars.chars().collect();
            s.trim_end_matches(|c| set.contains(&c)).to_owned()
        }
    };
    if let Some(same) = str_unchanged_self(args, &s, &out) {
        return Ok(same);
    }
    Ok(str_result(args, out))
}

/// `str.rsplit` on runs of whitespace, honouring `maxsplit` from the
/// right. Mirrors CPython: the *last* `maxsplit` whitespace runs split,
/// and the left remainder keeps its internal spacing.
fn str_rsplit_whitespace(s: &str, maxsplit: i64) -> Vec<Object> {
    use crate::unicode_case::is_space;
    if maxsplit < 0 {
        return s
            .split(is_space)
            .filter(|f| !f.is_empty())
            .map(Object::from_str)
            .collect();
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len();
    let mut out_rev: Vec<String> = Vec::new();
    let mut i = n;
    let mut splits = 0;
    while i > 0 {
        while i > 0 && is_space(chars[i - 1].1) {
            i -= 1;
        }
        if i == 0 {
            break;
        }
        let end_byte = if i < n { chars[i].0 } else { s.len() };
        if splits >= maxsplit {
            out_rev.push(s[..end_byte].to_string());
            break;
        }
        while i > 0 && !is_space(chars[i - 1].1) {
            i -= 1;
        }
        let start_byte = chars[i].0;
        out_rev.push(s[start_byte..end_byte].to_string());
        splits += 1;
    }
    out_rev.reverse();
    out_rev.into_iter().map(Object::from_str).collect()
}

fn str_rsplit(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    str_arity("rsplit", args, 0, 2)?;
    let s = str_self(args)?;
    let s = s.as_ref();
    let sep = arg_or_kw(args, 1, kwargs, "sep");
    let maxsplit = split_maxsplit(arg_or_kw(args, 2, kwargs, "maxsplit"))?;
    let wrap = |out: Vec<Object>| -> Vec<Object> {
        if args.iter().any(|o| matches!(o, Object::WStr(_))) || matches!(sep, Some(Object::WStr(_)))
        {
            out.into_iter()
                .map(|o| match o {
                    Object::Str(piece) => bridge_to_object(&piece),
                    other => other,
                })
                .collect()
        } else {
            out
        }
    };
    let out: Vec<Object> = match sep {
        None | Some(Object::None) => str_rsplit_whitespace(s, maxsplit),
        Some(sep_obj) => {
            let sep = str_arg_bridged(sep_obj)
                .ok_or_else(|| type_error("must be str or None, not other"))?;
            if sep.is_empty() {
                return Err(value_error("empty separator"));
            }
            // Always scan right-to-left: with overlapping separators the
            // match positions differ from a left scan ('aaa'.rsplit('aa')
            // is ['a', ''], not ['', 'a'] — test_userstring).
            let mut pieces: Vec<&str> = if maxsplit < 0 {
                let mut v: Vec<&str> = s.rsplit(&*sep).collect();
                v.reverse();
                v
            } else {
                let mut v: Vec<&str> = s
                    .rsplitn((maxsplit as usize).saturating_add(1), &*sep)
                    .collect();
                v.reverse();
                v
            };
            pieces.drain(..).map(Object::from_str).collect()
        }
    };
    Ok(Object::new_list(wrap(out)))
}

fn str_splitlines(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    str_arity("splitlines", args, 0, 1)?;
    let s = str_self(args)?;
    let keepends = arg_or_kw(args, 1, kwargs, "keepends")
        .map(Object::is_truthy)
        .unwrap_or(false);
    // CPython line boundaries (`str.splitlines`): LF, CR, CRLF, VT, FF,
    // FS, GS, RS, NEL, LINE SEPARATOR, PARAGRAPH SEPARATOR.
    let is_break = |c: char| {
        matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        )
    };
    let mut out: Vec<Object> = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if is_break(c) {
            let end_no_eol = i;
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = iter.peek() {
                    end = j + 1;
                    iter.next();
                }
            }
            let line = if keepends {
                &s[start..end]
            } else {
                &s[start..end_no_eol]
            };
            out.push(str_result(args, line.to_owned()));
            start = end;
        }
    }
    if start < s.len() {
        out.push(str_result(args, s[start..].to_owned()));
    }
    Ok(Object::new_list(out))
}

fn str_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("rfind", args, 1, 3)?;
    let s = str_self(args)?;
    let s = s.as_ref();
    let sub = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("rfind() expected str")),
    };
    let total_chars = str_char_len(s) as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(-1));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    let hay = &s[start_byte..end_byte];
    match hay.rfind(&*sub) {
        Some(byte_idx) => {
            let abs_byte = byte_idx + start_byte;
            Ok(Object::Int(byte_offset_to_char(s, abs_byte) as i64))
        }
        None => Ok(Object::Int(-1)),
    }
}

fn str_index(args: &[Object]) -> Result<Object, RuntimeError> {
    // Arity-check under this method's own name (`^index\b` —
    // test_userstring.test_find_etc_raise_correct_error_messages)
    // before delegating to the `find` engine.
    str_arity("index", args, 1, 3)?;
    let pos = str_find(args)?;
    match pos {
        Object::Int(-1) => Err(value_error("substring not found")),
        other => Ok(other),
    }
}

fn str_rindex(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("rindex", args, 1, 3)?;
    let pos = str_rfind(args)?;
    match pos {
        Object::Int(-1) => Err(value_error("substring not found")),
        other => Ok(other),
    }
}

fn str_count(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("count", args, 1, 3)?;
    let s = str_self(args)?;
    let s = s.as_ref();
    let sub = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("count() expected str")),
    };
    let total_chars = str_char_len(s) as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(0));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    // An empty needle matches at every code-point boundary (CPython counts
    // `len+1`); Rust's `matches("")` already yields that, but on the bridged
    // string each PUA char is one boundary, matching code-point semantics.
    Ok(Object::Int(
        s[start_byte..end_byte].matches(&*sub).count() as i64
    ))
}

fn str_partition(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let s = s.as_ref();
    let sep = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("partition() expected str")),
    };
    if sep.is_empty() {
        return Err(value_error("empty separator"));
    }
    let (head, tail) = match s.find(&*sep) {
        Some(i) => (s[..i].to_owned(), s[i + sep.len()..].to_owned()),
        None => {
            return Ok(Object::new_tuple(vec![
                str_result(args, s.to_owned()),
                Object::from_static(""),
                Object::from_static(""),
            ]))
        }
    };
    Ok(Object::new_tuple(vec![
        str_result(args, head),
        str_result(args, sep.into_owned()),
        str_result(args, tail),
    ]))
}

fn str_rpartition(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let s = s.as_ref();
    let sep = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("rpartition() expected str")),
    };
    if sep.is_empty() {
        return Err(value_error("empty separator"));
    }
    let (head, tail) = match s.rfind(&*sep) {
        Some(i) => (s[..i].to_owned(), s[i + sep.len()..].to_owned()),
        None => {
            return Ok(Object::new_tuple(vec![
                Object::from_static(""),
                Object::from_static(""),
                str_result(args, s.to_owned()),
            ]))
        }
    };
    Ok(Object::new_tuple(vec![
        str_result(args, head),
        str_result(args, sep.into_owned()),
        str_result(args, tail),
    ]))
}

fn str_isdigit(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isdigit", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_numeric::is_digit_char),
    ))
}

fn str_isnumeric(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isnumeric", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_numeric::is_numeric_char),
    ))
}

fn str_isdecimal(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isdecimal", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_numeric::is_decimal_char),
    ))
}

fn str_isalpha(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isalpha", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_case::is_alpha),
    ))
}

fn str_isalnum(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isalnum", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_case::is_alnum),
    ))
}

fn str_isspace(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isspace", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(crate::unicode_case::is_space),
    ))
}

fn str_isupper(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isupper", args, 0, 0)?;
    Ok(Object::Bool(crate::unicode_case::str_isupper(&str_self(
        args,
    )?)))
}

fn str_islower(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("islower", args, 0, 0)?;
    Ok(Object::Bool(crate::unicode_case::str_islower(&str_self(
        args,
    )?)))
}

fn str_istitle(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("istitle", args, 0, 0)?;
    Ok(Object::Bool(crate::unicode_case::str_istitle(&str_self(
        args,
    )?)))
}

fn str_isascii(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isascii", args, 0, 0)?;
    Ok(Object::Bool(str_self(args)?.is_ascii()))
}

fn str_isidentifier(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isidentifier", args, 0, 0)?;
    let s = str_self(args)?;
    let mut chars = s.chars();
    let valid = match chars.next() {
        Some(c) if c == '_' || crate::unicode_case::is_xid_start(c) => {
            chars.all(crate::unicode_case::is_xid_continue)
        }
        _ => false,
    };
    Ok(Object::Bool(valid))
}

fn str_isprintable(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("isprintable", args, 0, 0)?;
    let s = str_self(args)?;
    Ok(Object::Bool(
        s.chars().all(crate::object::char_is_printable),
    ))
}

fn str_zfill(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("zfill", args, 1, 1)?;
    let s = str_self(args)?;
    let s = s.as_ref();
    let width = match args.get(1) {
        // A negative width is a no-op in CPython (`'x'.zfill(-3) == 'x'`);
        // clamp to 0 so `*i as usize` can't wrap to a gigantic pad count.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        _ => return Err(type_error("zfill() expected int")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(str_result(args, s.to_owned()));
    }
    let pad = width - len;
    let (sign, rest) = if s.starts_with('+') || s.starts_with('-') {
        (&s[..1], &s[1..])
    } else {
        ("", s)
    };
    Ok(str_result(args, format!("{sign}{}{rest}", "0".repeat(pad))))
}

fn str_ljust(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("ljust", args, 1, 2)?;
    pad_str(args, false)
}

fn str_rjust(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("rjust", args, 1, 2)?;
    pad_str(args, true)
}

fn pad_str(args: &[Object], right_align: bool) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let width = match args.get(1) {
        // Negative widths are no-ops in CPython (`'x'.ljust(-3) == 'x'`);
        // clamp so the `as usize` cast can't underflow to a huge pad count.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        _ => return Err(type_error("expected int width")),
    };
    let fill = match args.get(2).map(str_arg_bridged) {
        Some(Some(f)) if f.chars().count() == 1 => f.chars().next().unwrap(),
        None => ' ',
        _ => return Err(type_error("fill must be single char")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(str_result(args, s.into_owned()));
    }
    let pad: String = std::iter::repeat_n(fill, width - len).collect();
    Ok(str_result(
        args,
        if right_align {
            format!("{pad}{s}")
        } else {
            format!("{s}{pad}")
        },
    ))
}

fn str_center(args: &[Object]) -> Result<Object, RuntimeError> {
    str_arity("center", args, 1, 2)?;
    let s = str_self(args)?;
    let width = match args.get(1) {
        // Negative widths are no-ops in CPython; clamp to avoid an `as usize`
        // underflow that would request a gigantic allocation.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        _ => return Err(type_error("center() expected int")),
    };
    let fill = match args.get(2).map(str_arg_bridged) {
        Some(Some(f)) if f.chars().count() == 1 => f.chars().next().unwrap(),
        None => ' ',
        _ => return Err(type_error("fill must be single char")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(str_result(args, s.into_owned()));
    }
    let total = width - len;
    // CPython biases the extra pad to the *left* when both the margin and the
    // width are odd (`marg / 2 + (marg & width & 1)`), so `'Monday'.center(9)`
    // is `'  Monday '`, not `' Monday  '`.
    let left = total / 2 + (total & width & 1);
    let right = total - left;
    let lpad: String = std::iter::repeat_n(fill, left).collect();
    let rpad: String = std::iter::repeat_n(fill, right).collect();
    Ok(str_result(args, format!("{lpad}{s}{rpad}")))
}

/// `str.expandtabs(tabsize=8)` — `tabsize` is positional-or-keyword in
/// CPython's clinic signature (test/string_tests.py passes it by name).
fn str_expandtabs_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mut full = args.to_vec();
    for (k, v) in kwargs {
        if k != "tabsize" {
            return Err(type_error(format!(
                "expandtabs() got an unexpected keyword argument '{k}'"
            )));
        }
        if full.len() > 1 {
            return Err(type_error(
                "argument for expandtabs() given by name ('tabsize') and position (1)",
            ));
        }
        full.push(v.clone());
    }
    str_expandtabs(&full)
}

fn str_expandtabs(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 2 {
        return Err(type_error(format!(
            "expandtabs() takes at most 1 argument ({} given)",
            args.len() - 1
        )));
    }
    let s = str_self(args)?;
    let tabsize = match args.get(1) {
        // A negative tabsize collapses tabs to zero columns in CPython
        // (`'x\ty'.expandtabs(-1) == 'xy'`); clamp so the `as usize` cast
        // can't wrap into a gigantic pad allocation.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        None => 8,
        _ => return Err(type_error("expandtabs() expected int")),
    };
    let mut out = String::new();
    let mut col = 0usize;
    for ch in s.chars() {
        match ch {
            '\t' => {
                let pad = if tabsize == 0 {
                    0
                } else {
                    tabsize - (col % tabsize)
                };
                for _ in 0..pad {
                    out.push(' ');
                }
                col += pad;
            }
            '\n' | '\r' => {
                out.push(ch);
                col = 0;
            }
            other => {
                out.push(other);
                col += 1;
            }
        }
    }
    Ok(str_result(args, out))
}

// CPython signature: `str.encode(encoding='utf-8', errors='strict')`; both
// are positional-or-keyword, so this must accept keywords (pandas' interchange
// buffer path and much stdlib call `s.encode(encoding=..., errors=...)`).
fn str_encode(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // Accept both `str` and surrogate-bearing `WStr` receivers so
    // `chr(0xD800).encode('utf-8', 'surrogatepass')` works.
    let recv = args
        .first()
        .ok_or_else(|| type_error("encode() missing receiver"))?;
    if !recv.is_str() {
        return Err(type_error("expected str method receiver"));
    }
    let encoding = match arg_or_kw(args, 1, kwargs, "encoding") {
        Some(Object::Str(e)) => e.to_string(),
        None => "utf-8".to_owned(),
        _ => return Err(type_error("encode() expected str")),
    };
    let errors = match arg_or_kw(args, 2, kwargs, "errors") {
        Some(Object::Str(e)) => e.to_string(),
        None => "strict".to_owned(),
        _ => "strict".to_owned(),
    };
    let bytes = crate::stdlib::codecs_mod::encode_obj(recv, &encoding, &errors)?;
    Ok(Object::new_bytes(bytes))
}

fn str_removeprefix(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "removeprefix() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    }
    let s = str_self(args)?;
    let s = s.as_ref();
    let prefix = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("removeprefix() expected str")),
    };
    let out = s.strip_prefix(&*prefix).unwrap_or(s).to_owned();
    Ok(str_result(args, out))
}

fn str_removesuffix(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "removesuffix() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    }
    let s = str_self(args)?;
    let s = s.as_ref();
    let suffix = match args.get(1).and_then(str_arg_bridged) {
        Some(p) => p,
        None => return Err(type_error("removesuffix() expected str")),
    };
    let out = s.strip_suffix(&*suffix).unwrap_or(s).to_owned();
    Ok(str_result(args, out))
}

/// Keyword-aware `str.format`. The VM's dispatch loop special-cases the
/// `.format` method and threads kwargs through `do_str_format`, so this body
/// only runs when the *bound method object* is invoked through the C-API
/// (`PyObject_Call` with a kwargs dict) — e.g. Cython building an error
/// message with `TEMPLATE.format(cls=..., own_freq=..., other_freq=...)`
/// (pandas' `DIFFERENT_FREQ`/`IncompatibleFrequency`). Without a `call_kw`
/// the C-API dispatch raised a spurious "format() takes no keyword arguments".
fn str_format_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let template = str_self(args)?.into_owned();
    let rest = &args[1..];
    crate::str_format_impl(&template, rest, kwargs).map(|s| str_result(args, s))
}

fn str_format_map(args: &[Object]) -> Result<Object, RuntimeError> {
    let template = str_self(args)?.into_owned();
    let mapping = match args.get(1) {
        Some(Object::Dict(d)) => d.clone(),
        _ => return Err(type_error("format_map() argument must be a mapping")),
    };
    crate::str_format_map_impl(&template, &mapping).map(|s| str_result(args, s))
}

fn str_translate(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    // CPython's `str.translate(table)` accepts *any* object supporting
    // `table[ord(c)]`: a `dict` (the `str.maketrans` case), or a sequence
    // indexed by ordinal (`email.quoprimime` passes a 256-entry `list`).
    // A missing key / out-of-range index leaves the character unchanged; a
    // value of `None` deletes it.
    enum Table {
        Dict(Object),
        Seq(Vec<Object>),
    }
    let table = match args.get(1) {
        Some(d @ Object::Dict(_)) => Table::Dict(d.clone()),
        Some(Object::List(l)) => Table::Seq(l.borrow().clone()),
        Some(Object::Tuple(t)) => Table::Seq(t.to_vec()),
        _ => {
            return Err(type_error(
                "translate() argument must be a mapping or sequence",
            ))
        }
    };
    let mut out = String::new();
    let receiver_bridged = matches!(args.first(), Some(Object::WStr(_)));
    let mut saw_surrogate = receiver_bridged;
    // Push a translation target code point, bridging a surrogate so it
    // round-trips through `str_result`.
    let push_cp = |out: &mut String, cp: u32, saw: &mut bool| {
        let mapped = if (0xD800..=0xDFFF).contains(&cp) {
            *saw = true;
            BRIDGE_BASE + (cp - 0xD800)
        } else {
            cp
        };
        if let Some(ch) = char::from_u32(mapped) {
            out.push(ch);
        }
    };
    for c in s.chars() {
        // Recover the real code point of a bridged surrogate for the lookup —
        // but only when the receiver actually travelled through the bridge
        // (`WStr`). A plain `str` holding a genuine plane-16 code point
        // (U+10FFFF) must look it up as-is (test_str.test_maketrans_translate).
        let cp = c as u32;
        let real_cp = if receiver_bridged && bridge_window(cp) {
            0xD800 + (cp - BRIDGE_BASE)
        } else {
            cp
        };
        let entry = match &table {
            Table::Dict(Object::Dict(d)) => d
                .borrow()
                .get(&DictKey(Object::Int(i64::from(real_cp))))
                .cloned(),
            Table::Dict(_) => None,
            Table::Seq(v) => v.get(real_cp as usize).cloned(),
        };
        match entry {
            Some(Object::None) => {}
            Some(Object::Int(i)) => {
                // CPython: an out-of-range target ordinal is a ValueError.
                if !(0..0x11_0000).contains(&i) {
                    return Err(crate::error::value_error(
                        "character mapping must be in range(0x110000)",
                    ));
                }
                push_cp(&mut out, i as u32, &mut saw_surrogate)
            }
            Some(Object::Str(v)) => out.push_str(&v),
            Some(Object::WStr(cps)) => {
                saw_surrogate = true;
                out.push_str(&bridge_encode_cps(&cps));
            }
            // `str` subclass value: fall back to its text view. Any other
            // type is a TypeError (CPython `charmaptranslate_lookup`).
            Some(other) => match other.native_value() {
                Some(Object::Str(v)) => out.push_str(&v),
                _ => {
                    return Err(type_error(
                        "character mapping must return integer, None or str",
                    ))
                }
            },
            None => out.push(c),
        }
    }
    Ok(if saw_surrogate {
        bridge_to_object(&out)
    } else {
        Object::from_str(out)
    })
}

fn str_maketrans(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut d = DictData::default();
    match args.len() {
        1 => match &args[0] {
            Object::Dict(map) => {
                for (k, v) in map.borrow().iter() {
                    let key = match &k.0 {
                        // CPython requires exactly one character per string
                        // key (`maketrans({'xy': 2})` is ValueError).
                        Object::Str(s) => {
                            let mut chars = s.chars();
                            match (chars.next(), chars.next()) {
                                (Some(c), None) => DictKey(Object::Int(i64::from(u32::from(c)))),
                                _ => {
                                    return Err(value_error(
                                        "string keys in translate table must be of length 1",
                                    ))
                                }
                            }
                        }
                        Object::Int(_) => k.clone(),
                        _ => {
                            return Err(type_error(
                                "keys in translate table must be strings or integers",
                            ))
                        }
                    };
                    d.insert(key, v.clone());
                }
            }
            _ => return Err(type_error("maketrans expected dict")),
        },
        2 | 3 => {
            let from = match &args[0] {
                Object::Str(s) => s.to_string(),
                _ => return Err(type_error("maketrans expected str")),
            };
            let to = match &args[1] {
                Object::Str(s) => s.to_string(),
                _ => return Err(type_error("maketrans expected str")),
            };
            if from.chars().count() != to.chars().count() {
                return Err(value_error(
                    "the first two maketrans arguments must have equal length",
                ));
            }
            for (a, b) in from.chars().zip(to.chars()) {
                d.insert(
                    DictKey(Object::Int(i64::from(u32::from(a)))),
                    Object::Int(i64::from(u32::from(b))),
                );
            }
            match args.get(2) {
                Some(Object::Str(rm)) => {
                    for c in rm.chars() {
                        d.insert(DictKey(Object::Int(i64::from(u32::from(c)))), Object::None);
                    }
                }
                None => {}
                Some(other) => {
                    return Err(type_error(format!(
                        "maketrans() argument 3 must be str, not {}",
                        other.type_name()
                    )))
                }
            }
        }
        _ => return Err(type_error("maketrans expected 1-3 arguments")),
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

// ---------- list methods ----------

fn list_self(args: &[Object]) -> Result<Rc<RefCell<Vec<Object>>>, RuntimeError> {
    match args.first() {
        Some(Object::List(l)) => Ok(l.clone()),
        // A subclass of `list` (`class C(list)`) carries its items in the
        // wrapped native payload. Unbound calls — `list.append(c, x)`,
        // `super().append(x)` — pass the instance, so unwrap it here.
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::List(l)) => Ok(l.clone()),
            _ => Err(type_error("expected list method receiver")),
        },
        _ => Err(type_error("expected list method receiver")),
    }
}

fn list_append(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.append() expected 1 argument"));
    }
    list_self(args)?.borrow_mut().push(args[1].clone());
    Ok(Object::None)
}

// List dunders exposed on the type so `list.__setitem__` /
// `super().__getitem__` resolve for `list` subclasses (`class C(list)`).
// These mirror CPython's `mp_subscript`/`mp_ass_subscript` slots fully:
// both integer and slice keys work (`_HashedSeq.__init__` does
// `self[:] = tup` on a `list` subclass, which dispatches here now that
// the materialized `__setitem__` is in the type dict).
fn list_index_arg(l_len: usize, idx: &Object, what: &str) -> Result<usize, RuntimeError> {
    match idx {
        Object::Int(i) => {
            let len = l_len as i64;
            let n = if *i < 0 { i + len } else { *i };
            if n < 0 || n >= len {
                Err(index_error("list index out of range"))
            } else {
                Ok(n as usize)
            }
        }
        Object::Bool(b) => list_index_arg(l_len, &Object::Int(i64::from(*b)), what),
        // Honour the full `__index__` protocol (CPython `PyNumber_Index`) —
        // a numpy integer scalar indexing through `super().__getitem__` on a
        // list subclass (pandas' `FrozenList._reorder_ilevels`).
        idx @ (Object::Instance(_) | Object::Foreign(_)) => match try_coerce_index_i64(idx) {
            Some(res) => list_index_arg(l_len, &Object::Int(res?), what),
            None => Err(type_error(format!(
                "list indices must be integers or slices, not {}",
                idx.type_name()
            ))),
        },
        _ => Err(type_error(format!(
            "list indices must be integers or slices, not {}",
            idx.type_name()
        ))),
    }
}

fn list_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__getitem__ expected 1 argument"))?;
    if let Object::Slice(s) = key {
        let seq = l.borrow().clone();
        return Ok(Object::new_list(crate::slice_seq(&seq, s)?));
    }
    let l = l.borrow();
    let n = list_index_arg(l.len(), key, "__getitem__")?;
    Ok(l[n].clone())
}

fn list_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__setitem__ expected 2 arguments"))?;
    let val = args
        .get(2)
        .ok_or_else(|| type_error("__setitem__ expected 2 arguments"))?;
    if let Object::Slice(s) = key {
        // Materialize the replacement *before* the mutable borrow so
        // self-assignment (`l[:] = l`) can't alias the live borrow.
        let mut replacement = Vec::new();
        let mut it = val.make_iter()?;
        while let Some(v) = it.next_value() {
            replacement.push(v);
        }
        let replaced = crate::apply_slice_assignment(&mut l.borrow_mut(), s, replacement)?;
        for old in replaced {
            queue_removed(old);
        }
        return Ok(Object::None);
    }
    let old = {
        let mut l = l.borrow_mut();
        let n = list_index_arg(l.len(), key, "__setitem__")?;
        std::mem::replace(&mut l[n], val.clone())
    };
    queue_removed(old);
    Ok(Object::None)
}

fn list_delitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__delitem__ expected 1 argument"))?;
    if let Object::Slice(s) = key {
        let mut l = l.borrow_mut();
        let mut indices = crate::slice_indices(l.len(), s)?;
        indices.sort_unstable();
        for i in indices.into_iter().rev() {
            queue_removed(l.remove(i));
        }
        return Ok(Object::None);
    }
    let removed = {
        let mut l = l.borrow_mut();
        let n = list_index_arg(l.len(), key, "__delitem__")?;
        l.remove(n)
    };
    queue_removed(removed);
    Ok(Object::None)
}

fn list_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    // `list.pop(index=-1)` accepts at most one positional argument
    // (`[].pop(1, 2)` is a TypeError in CPython, not an IndexError).
    if args.len() > 2 {
        return Err(type_error(format!(
            "pop expected at most 1 argument, got {}",
            args.len() - 1
        )));
    }
    let l = list_self(args)?;
    let mut l = l.borrow_mut();
    let idx = if args.len() > 1 {
        match &args[1] {
            Object::Int(i) => {
                if l.is_empty() {
                    return Err(index_error("pop from empty list"));
                }
                let len = l.len() as i64;
                let n = if *i < 0 { i + len } else { *i };
                if n < 0 || n >= len {
                    return Err(index_error("pop index out of range"));
                }
                n as usize
            }
            _ => return Err(type_error("pop index must be int")),
        }
    } else {
        if l.is_empty() {
            return Err(index_error("pop from empty list"));
        }
        l.len() - 1
    };
    Ok(l.remove(idx))
}

fn list_extend(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.extend() expected 1 argument"));
    }
    let l = list_self(args)?;
    // Fast path for exact list/tuple sources: snapshot the source items
    // first. CPython reads the source's length once, so `a.extend(a)`
    // *doubles* `a`; iterating the live list while pushing to it would
    // instead grow without bound (a hang that the OOM killer ends).
    match &args[1] {
        Object::List(src) => {
            let items: Vec<Object> = src.borrow().clone();
            l.borrow_mut().extend(items);
            return Ok(Object::None);
        }
        Object::Tuple(src) => {
            let items: Vec<Object> = src.iter().cloned().collect();
            l.borrow_mut().extend(items);
            return Ok(Object::None);
        }
        _ => {}
    }
    let mut it = args[1].make_iter()?;
    while let Some(v) = it.next_value() {
        l.borrow_mut().push(v);
    }
    Ok(Object::None)
}

/// `list.__iadd__(self, other)` — extend in place, return self
/// (CPython `list_inplace_concat`; accepts any iterable).
fn list_iadd(args: &[Object]) -> Result<Object, RuntimeError> {
    list_extend(args)?;
    Ok(args[0].clone())
}

/// `list.__imul__(self, n)` — repeat in place, return self.
fn list_imul(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.__imul__() expected 1 argument"));
    }
    let n = match &args[1] {
        Object::Int(i) => *i,
        Object::Bool(b) => i64::from(*b),
        // Sequence repetition honours `__index__` (CPython `sq_repeat`
        // via `PyNumber_AsSsize_t` — test_index.test_inplace_repeat).
        other => match try_coerce_index_i64(other) {
            Some(res) => res?,
            None => {
                return Err(type_error(format!(
                    "can't multiply sequence by non-int of type '{}'",
                    other.type_name()
                )))
            }
        },
    };
    let l = list_self(args)?;
    let mut data = l.borrow_mut();
    if n <= 0 {
        data.clear();
    } else {
        let unit = data.len();
        let times = crate::checked_repeat_count(unit, n, "list")?;
        let extra = unit.saturating_mul(times.saturating_sub(1));
        if data.try_reserve_exact(extra).is_err() {
            return Err(crate::error::memory_error(""));
        }
        let original = data.clone();
        for _ in 1..n {
            data.extend_from_slice(&original);
        }
    }
    drop(data);
    Ok(args[0].clone())
}

fn list_insert(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 3 {
        return Err(type_error("list.insert() expected 2 arguments"));
    }
    let i = match &args[1] {
        Object::Int(i) => *i,
        _ => return Err(type_error("insert index must be int")),
    };
    let l = list_self(args)?;
    let mut l = l.borrow_mut();
    let len = l.len() as i64;
    let idx = if i < 0 {
        (i + len).max(0) as usize
    } else if i > len {
        l.len()
    } else {
        i as usize
    };
    l.insert(idx, args[2].clone());
    Ok(Object::None)
}

fn list_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.remove() expected 1 argument"));
    }
    let l = list_self(args)?;
    // CPython `list.remove` walks the *live* list by index with
    // `PyObject_RichCompareBool` (identity-first, then Python `__eq__`),
    // so `ALWAYS_EQ` matches the first element and a mutating `__eq__`
    // can't panic the borrowed cell: clone each element and release the
    // borrow before comparing.
    let mut i = 0usize;
    loop {
        let x = {
            let b = l.borrow();
            if i >= b.len() {
                break;
            }
            b[i].clone()
        };
        if crate::object::member_eq(&x, &args[1])? {
            let removed = {
                let mut b = l.borrow_mut();
                if i < b.len() {
                    Some(b.remove(i))
                } else {
                    None
                }
            };
            if let Some(removed) = removed {
                queue_removed(removed);
            }
            return Ok(Object::None);
        }
        i += 1;
    }
    Err(value_error("list.remove(x): x not in list"))
}

fn list_index(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("list.index() expected at least 1 argument"));
    }
    // Snapshot strong refs: CPython `list.index` re-reads by index because a
    // user `__eq__` can mutate the list mid-scan; cloning the handles keeps us
    // panic-free without holding the borrow across the comparison.
    let items: Vec<Object> = list_self(args)?.borrow().clone();
    // CPython `list.index(value, start=0, stop=maxsize)`: negative bounds count
    // from the end and clamp to 0 (`PySlice_AdjustIndices` semantics), and the
    // comparison is `PyObject_RichCompareBool` (identity-first, then Python
    // `__eq__` both directions, propagating exceptions).
    let len = items.len() as i64;
    let adjust = |v: i64| -> i64 {
        if v < 0 {
            (v + len).max(0)
        } else {
            v.min(len)
        }
    };
    let start = match args.get(2) {
        Some(o) => adjust(seq_index_bound(o)?),
        None => 0,
    };
    let stop = match args.get(3) {
        Some(o) => adjust(seq_index_bound(o)?),
        None => len,
    };
    let mut i = start;
    while i < stop {
        if crate::object::member_eq(&items[i as usize], &args[1])? {
            return Ok(Object::Int(i));
        }
        i += 1;
    }
    Err(value_error(format!("{} is not in list", args[1].repr())))
}

fn list_count(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.count() expected 1 argument"));
    }
    // Snapshot strong refs (a user `__eq__` may mutate the list mid-scan).
    // CPython compares with `PyObject_RichCompareBool`: identity-first (so
    // `[nan].count(nan)` for the same nan is 1), then Python `__eq__` both
    // directions, letting any `__eq__` exception propagate.
    let items: Vec<Object> = list_self(args)?.borrow().clone();
    let mut n: i64 = 0;
    for x in &items {
        if crate::object::member_eq(x, &args[1])? {
            n += 1;
        }
    }
    Ok(Object::Int(n))
}

// ---- range.index / range.count -------------------------------------------
//
// CPython's `range_index`/`range_count` take an arithmetic fast path for
// real integers (`PyLong`/`bool`) and fall back to a linear
// `_PySequence_IterSearch` for anything else (a float that equals an int, a
// numpy scalar). We mirror both.

fn range_self(args: &[Object], meth: &str) -> Result<Rc<crate::object::Range>, RuntimeError> {
    match args.first() {
        Some(Object::Range(r)) => Ok(r.clone()),
        _ => Err(type_error(format!(
            "descriptor '{meth}' for 'range' objects doesn't apply to the given object"
        ))),
    }
}

fn range_len_i128(r: &crate::object::Range) -> i128 {
    if r.step > 0 {
        if r.stop > r.start {
            (r.stop - r.start + r.step - 1) / r.step
        } else {
            0
        }
    } else if r.stop < r.start {
        (r.start - r.stop + (-r.step) - 1) / (-r.step)
    } else {
        0
    }
}

/// The 0-based position of integer `v` within `r`, or `None` if `v` is not a
/// member. Mirrors CPython's `range_contains_long` + index arithmetic.
fn range_position(r: &crate::object::Range, v: i128) -> Option<i128> {
    if r.big.is_some() {
        return None; // callers route big-bounded ranges to `range_position_big`
    }
    if r.step > 0 {
        if v < r.start || v >= r.stop {
            return None;
        }
    } else if v > r.start || v <= r.stop {
        return None;
    }
    let diff = v - r.start;
    if diff % r.step != 0 {
        return None;
    }
    Some(diff / r.step)
}

/// [`range_position`] at full precision, for big-bounded ranges.
fn range_position_big(r: &crate::object::Range, v: &BigInt) -> Option<BigInt> {
    let (start, stop, step) = r.bounds();
    let zero = BigInt::from(0);
    if step > zero {
        if *v < start || *v >= stop {
            return None;
        }
    } else if *v > start || *v <= stop {
        return None;
    }
    let diff = v - &start;
    if &diff % &step != zero {
        return None;
    }
    Some(diff / step)
}

fn int_like_to_i128(o: &Object) -> Option<i128> {
    match o {
        Object::Int(i) => Some(i128::from(*i)),
        Object::Bool(b) => Some(i128::from(*b)),
        Object::Long(b) => b.to_i128(),
        _ => None,
    }
}

fn int_like_to_bigint(o: &Object) -> Option<BigInt> {
    match o {
        Object::Int(i) => Some(BigInt::from(*i)),
        Object::Bool(b) => Some(BigInt::from(i64::from(*b))),
        Object::Long(b) => Some((**b).clone()),
        _ => None,
    }
}

/// `range.__getitem__(self, index)` — int (through `__index__`) and
/// slice subscription, mirroring CPython's `range_subscript`.
fn range_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let r = match args.first() {
        Some(Object::Range(r)) => r.clone(),
        _ => {
            return Err(type_error(
                "descriptor '__getitem__' requires a 'range' object",
            ))
        }
    };
    let index = args
        .get(1)
        .ok_or_else(|| type_error("__getitem__() takes exactly one argument (0 given)"))?;
    let len = crate::object::range_len_bigint(&r);
    if let Object::Slice(slc) = index {
        return crate::range_slice(&r, len, slc);
    }
    let i = coerce_index_bigint(&coerce_index_object(index)?)?;
    let zero = BigInt::from(0);
    let idx = if i < zero { i + &len } else { i };
    if idx < zero || idx >= len {
        return Err(crate::error::index_error("range object index out of range"));
    }
    let (start, _, step) = r.bounds();
    Ok(Object::int_from_bigint(start + idx * step))
}

fn range_index(args: &[Object]) -> Result<Object, RuntimeError> {
    let r = range_self(args, "index")?;
    let value = args
        .get(1)
        .ok_or_else(|| type_error("index() takes exactly one argument (0 given)"))?;
    if r.big.is_some() {
        if let Some(v) = int_like_to_bigint(value) {
            if let Some(idx) = range_position_big(&r, &v) {
                return Ok(Object::int_from_bigint(idx));
            }
            return Err(value_error(format!("{} is not in range", value.repr())));
        }
    } else if let Some(v) = int_like_to_i128(value) {
        if let Some(idx) = range_position(&r, v) {
            return Ok(crate::object::int_from_i128(idx));
        }
        return Err(value_error(format!("{} is not in range", value.repr())));
    }
    // Non-integer: linear search comparing each element with `__eq__`
    // (CPython `_PySequence_IterSearch(..., PY_ITERSEARCH_INDEX)`).
    let n = range_len_i128(&r);
    let mut k: i128 = 0;
    while k < n {
        let elem = crate::object::int_from_i128(r.start + k * r.step);
        if crate::object::member_eq(&elem, value)? {
            return Ok(crate::object::int_from_i128(k));
        }
        k += 1;
    }
    Err(value_error(format!("{} is not in range", value.repr())))
}

fn range_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let r = range_self(args, "count")?;
    let value = args
        .get(1)
        .ok_or_else(|| type_error("count() takes exactly one argument (0 given)"))?;
    if r.big.is_some() {
        if let Some(v) = int_like_to_bigint(value) {
            return Ok(Object::Int(i64::from(range_position_big(&r, &v).is_some())));
        }
    } else if let Some(v) = int_like_to_i128(value) {
        return Ok(Object::Int(i64::from(range_position(&r, v).is_some())));
    }
    let n = range_len_i128(&r);
    let mut cnt: i64 = 0;
    let mut k: i128 = 0;
    while k < n {
        let elem = crate::object::int_from_i128(r.start + k * r.step);
        if crate::object::member_eq(&elem, value)? {
            cnt += 1;
        }
        k += 1;
    }
    Ok(Object::Int(cnt))
}

fn list_sort(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let mut err: Option<RuntimeError> = None;
    l.borrow_mut()
        .sort_by(|a: &Object, b: &Object| match a.cmp(b) {
            Ok(o) => o,
            Err(e) => {
                err = Some(e);
                std::cmp::Ordering::Equal
            }
        });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(Object::None)
}

fn list_reverse(args: &[Object]) -> Result<Object, RuntimeError> {
    // `list.reverse()` is `METH_NOARGS`: extra arguments are a TypeError.
    if args.len() != 1 {
        return Err(type_error("reverse() takes no arguments (1 given)"));
    }
    list_self(args)?.borrow_mut().reverse();
    Ok(Object::None)
}

fn list_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 1 {
        return Err(type_error("clear() takes no arguments (1 given)"));
    }
    let evicted: Vec<Object> = std::mem::take(&mut *list_self(args)?.borrow_mut());
    for v in evicted {
        queue_removed(v);
    }
    Ok(Object::None)
}

fn list_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 1 {
        return Err(type_error("copy() takes no arguments (1 given)"));
    }
    let l = list_self(args)?;
    let v = l.borrow().clone();
    Ok(Object::new_list(v))
}

// ---------- dict methods ----------

/// `dict.__iter__(self)` → a key iterator (CPython's `dict_iter`), so
/// `iter(d)` parity holds when the dunder is fetched explicitly.
fn dict_iter_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__iter__() missing self"))?;
    let it = recv.make_iter()?;
    Ok(Object::Iter(Rc::new(RefCell::new(it))))
}

fn dict_self(args: &[Object]) -> Result<Rc<RefCell<DictData>>, RuntimeError> {
    match args.first() {
        Some(Object::Dict(d)) => Ok(d.clone()),
        // A subclass of `dict` (`class C(dict)`) carries its entries in the
        // wrapped native payload. Unbound calls — `dict.__setitem__(c, k, v)`,
        // `super().__setitem__(k, v)` — pass the instance, so unwrap it here.
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::Dict(d)) => Ok(d.clone()),
            _ => Err(type_error("expected dict method receiver")),
        },
        _ => Err(type_error("expected dict method receiver")),
    }
}

fn dict_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    // `dict.get(key[, default])` — CPython rejects a third positional.
    if args.len() > 3 {
        return Err(type_error(format!(
            "get expected at most 2 arguments, got {}",
            args.len() - 1
        )));
    }
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.get() expected at least 1 argument"))?;
    ensure_hashable(key)?;
    let default = args.get(2).cloned().unwrap_or(Object::None);
    let value = dict_lookup(&d, key)?.unwrap_or(default);
    Ok(value)
}

/// `d[key]` probe honouring re-entrant user `__eq__`/`__hash__`: a key whose
/// comparison can run Python takes the borrow-free two-phase path (its
/// `__eq__` may mutate `d` mid-lookup — gh-140551) and propagates exceptions
/// those dunders raise; every other key uses the direct native probe.
pub(crate) fn dict_lookup(
    d: &Rc<RefCell<DictData>>,
    key: &Object,
) -> Result<Option<Object>, RuntimeError> {
    if crate::object::dict_key_is_reentrant(key) {
        return crate::object::dict_reentrant_get(d, key);
    }
    // `key_cmp_scope` re-raises an exception a Python `__hash__` (or a
    // stored key's `__eq__`) parked during the infallible table probe
    // (test_dict `BadHash`: `d[x]` must surface the `__hash__` error).
    let (found, deferred) = crate::object::with_key_eq_deferred(|| {
        crate::object::key_cmp_scope(|| d.borrow().get(&DictKey(key.clone())).cloned())
    });
    match found? {
        Some(v) => Ok(Some(v)),
        // A stored key needed a Python comparison: retry borrow-free.
        None if deferred => crate::object::dict_reentrant_get(d, key),
        None => Ok(None),
    }
}

/// `d[key] = value` honouring re-entrant keys (see [`dict_lookup`]).
/// Returns the replaced value, if any.
pub(crate) fn dict_insert(
    d: &Rc<RefCell<DictData>>,
    key: Object,
    value: Object,
) -> Result<Option<Object>, RuntimeError> {
    if crate::object::dict_key_is_reentrant(&key) {
        return crate::object::dict_reentrant_insert(d, key, value);
    }
    let (old, deferred) = crate::object::with_key_eq_deferred(|| {
        crate::object::key_cmp_scope(|| d.borrow_mut().insert(DictKey(key.clone()), value.clone()))
    });
    let old = match &old {
        Err(_) => {
            // The probe raised (a `__hash__`/`__eq__` error). The entry may
            // have been appended before the error was noticed; evict it so a
            // failed insert leaves the dict untouched (CPython aborts the
            // whole `PyDict_SetItem`).
            let mut m = d.borrow_mut();
            if m.keys().next_back().is_some_and(|k| k.0.is_same(&key)) {
                m.pop();
            }
            drop(m);
            return old;
        }
        Ok(v) => v.clone(),
    };
    match old {
        // Replaced natively — genuine equality, no Python needed.
        Some(old) => {
            // A value replaced by a *different* object is an effective
            // mutation (PEP 509 / builtins-watch); re-storing the identical
            // object is not (test_dict_version.test_setitem_same_value).
            if !old.is_same(&value) {
                crate::object::dict_mutation_event(d);
            }
            // PyDict_Watch: MODIFIED fires even when the stored object is
            // identical (CPython notifies before comparing; test_watchers
            // test_object_dict stores the same literal 100×).
            if crate::capi_watchers::dicts_active() {
                crate::capi_watchers::dict_event("MODIFIED", d, Some(&key), Some(&value));
            }
            Ok(Some(old))
        }
        None if deferred => {
            // Appended while a stored key still needed a Python comparison:
            // undo the append (`insert` places new keys last) and redo on
            // the borrow-free path.
            {
                let mut m = d.borrow_mut();
                let popped = m.pop();
                debug_assert!(popped.is_some_and(|(k, _)| k.0.is_same(&key)));
            }
            crate::object::dict_reentrant_insert(d, key, value)
        }
        None => {
            // A new key landed: notify any live iterator watching `d`
            // (the "keys changed during iteration" trip-wire). Value
            // overwrites (the `Some` arm above) intentionally don't.
            crate::object::dict_watch_bump(d);
            crate::object::dict_mutation_event(d);
            if crate::capi_watchers::dicts_active() {
                crate::capi_watchers::dict_event("ADDED", d, Some(&key), Some(&value));
            }
            Ok(None)
        }
    }
}

/// `del d[key]` / `d.pop(key)` honouring re-entrant keys (see
/// [`dict_lookup`]). Returns the evicted `(stored key, value)` pair.
pub(crate) fn dict_remove(
    d: &Rc<RefCell<DictData>>,
    key: &Object,
) -> Result<Option<(Object, Object)>, RuntimeError> {
    if crate::object::dict_key_is_reentrant(key) {
        return crate::object::dict_reentrant_remove(d, key);
    }
    let (removed, deferred) = crate::object::with_key_eq_deferred(|| {
        crate::object::key_cmp_scope(|| {
            d.borrow_mut()
                .shift_remove_entry(&DictKey(key.clone()))
                .map(|(k, v)| (k.0, v))
        })
    });
    match removed? {
        Some(entry) => {
            crate::object::dict_watch_bump(d);
            crate::object::dict_mutation_event(d);
            if crate::capi_watchers::dicts_active() {
                crate::capi_watchers::dict_event("DELETED", d, Some(key), None);
            }
            Ok(Some(entry))
        }
        None if deferred => crate::object::dict_reentrant_remove(d, key),
        None => Ok(None),
    }
}

// Container dunders exposed on the type so `dict.__setitem__`,
// `super().__getitem__`, … resolve for `dict` subclasses. They mirror the
// VM's subscript opcodes but operate on the (possibly unwrapped) native
// payload. `__init__` reuses `dict_update` (clear-then-fill is unnecessary:
// a freshly constructed subclass starts with an empty native dict).
fn dict_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__setitem__ expected 2 arguments"))?;
    let val = args
        .get(2)
        .ok_or_else(|| type_error("__setitem__ expected 2 arguments"))?;
    ensure_hashable(key)?;
    let old = dict_insert(&d, key.clone(), val.clone())?;
    if let Some(old) = old {
        queue_removed(old);
    }
    Ok(Object::None)
}

fn dict_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__getitem__ expected 1 argument"))?;
    ensure_hashable(key)?;
    let found = dict_lookup(&d, key)?;
    // CPython's `KeyError` carries the missing key *object* as `args[0]`
    // (`e.args[0] is key`), not its repr string; `str(e)` still renders
    // `repr(key)`. A `dict` subclass's `__missing__` also receives this
    // exact object when the bound-method path re-raises.
    found.ok_or_else(|| key_error_object(key.clone()))
}

/// Route a key/value evicted by a container mutator to the prompt-reap
/// queue (see [`crate::vm_singletons::queue_container_removed`]) before
/// dropping our reference. The queue holds a clone; the eval loop reaps it
/// at the next between-bytecodes safe point if the eviction was its last
/// program-visible reference.
pub(crate) fn queue_removed(v: Object) {
    crate::vm_singletons::queue_container_removed(&v);
}

fn dict_delitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__delitem__ expected 1 argument"))?;
    ensure_hashable(key)?;
    let removed = dict_remove(&d, key)?;
    if let Some((k, v)) = removed {
        queue_removed(k);
        queue_removed(v);
        Ok(Object::None)
    } else {
        Err(key_error_object(key.clone()))
    }
}

/// `dict.keys()`/`values()`/`items()` take no positional arguments beyond
/// `self`; CPython raises `TypeError` for any extra (`mapping_tests` checks
/// `d.keys(None)`).
fn dict_view_no_args(args: &[Object], name: &str) -> Result<(), RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "{name}() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    Ok(())
}

/// The view's keepalive owner: a dict-*subclass* receiver must stay
/// alive while the view (and any iterator over it) lives — CPython's
/// views hold the dict object itself, not just its storage
/// (test_dict test_free_after_iterating).
fn dict_view_owner(args: &[Object]) -> Option<Object> {
    match args.first() {
        Some(o @ Object::Instance(_)) => Some(o.clone()),
        _ => None,
    }
}

fn dict_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "keys")?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Keys,
        owner: dict_view_owner(args),
    })))
}

fn dict_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "values")?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Values,
        owner: dict_view_owner(args),
    })))
}

fn dict_items(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "items")?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Items,
        owner: dict_view_owner(args),
    })))
}

fn dict_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.pop() expected at least 1 argument"))?;
    ensure_hashable(key)?;
    let removed = dict_remove(&d, key)?;
    if let Some((k, v)) = removed {
        // The *stored* key (equal to, but possibly distinct from, the
        // lookup key) is evicted too; the value is returned to the caller.
        queue_removed(k);
        Ok(v)
    } else if let Some(default) = args.get(2).cloned() {
        Ok(default)
    } else {
        Err(key_error_object(key.clone()))
    }
}

fn dict_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    if let Some(other) = args.get(1) {
        // PyDict_Watch: merging a plain dict into an empty watched dict is
        // CPython's clone fast path — one CLONED event, no per-key events
        // (test_watchers test_clone). The guard suppresses the per-key
        // dispatch from `dict_insert` for the duration of the merge.
        let _suppress = if crate::capi_watchers::dicts_active() && matches!(other, Object::Dict(_))
        {
            let was_empty = d.borrow().is_empty();
            Some(crate::capi_watchers::dict_update_begin(&d, was_empty))
        } else {
            None
        };
        match other {
            Object::Dict(o) => {
                // Snapshot the source into a temporary first so we
                // don't hold a borrow on `o` while reaching for
                // `d.borrow_mut()`. The two may alias (e.g.
                // `d.update(d)`), and even if they don't, our
                // GilCell forbids overlapping borrows when source
                // and destination share storage.
                let entries: Vec<(DictKey, Object)> = o
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let src_len = entries.len();
                for (k, v) in entries {
                    if let Some(old) = dict_insert(&d, k.0, v)? {
                        queue_removed(old);
                    }
                    // CPython's `PyDict_Merge` re-checks the source size
                    // *after* every insert: a key `__eq__` that mutates the
                    // source mid-merge aborts — including on the final entry
                    // (test_dict `test_merge_and_mutate` puts the mutating
                    // key last).
                    if o.borrow().len() != src_len {
                        return Err(crate::error::runtime_error("dict mutated during update"));
                    }
                }
            }
            Object::MappingProxy(o) => {
                let entries: Vec<(DictKey, Object)> = o
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let mut dst = d.borrow_mut();
                for (k, v) in entries {
                    if let Some(old) = dst.insert(k, v) {
                        queue_removed(old);
                    }
                }
            }
            _ => return Err(type_error("dict.update() expected dict")),
        }
    }
    Ok(Object::None)
}

fn dict_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "clear")?;
    let evicted: Vec<(DictKey, Object)> = d.borrow_mut().drain(..).collect();
    if !evicted.is_empty() {
        crate::object::dict_watch_bump(&d);
        crate::object::dict_mutation_event(&d);
        if crate::capi_watchers::dicts_active() {
            crate::capi_watchers::dict_event("CLEARED", &d, None, None);
        }
    }
    for (k, v) in evicted {
        queue_removed(k.0);
        queue_removed(v);
    }
    Ok(Object::None)
}

// ---------- tuple methods ----------

fn tuple_count(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("tuple.count() expected 1 argument"));
    }
    let t = match args.first() {
        Some(Object::Tuple(t)) => t.clone(),
        _ => return Err(type_error("expected tuple")),
    };
    // `PyObject_RichCompareBool`: identity-first, then Python `__eq__` (both
    // directions), propagating any exception the comparison raises.
    let mut n: i64 = 0;
    for x in t.iter() {
        if crate::object::member_eq(x, &args[1])? {
            n += 1;
        }
    }
    Ok(Object::Int(n))
}

fn tuple_index(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("tuple.index() expected at least 1 argument"));
    }
    let t = match args.first() {
        Some(Object::Tuple(t)) => t.clone(),
        _ => return Err(type_error("expected tuple")),
    };
    // Same `(value, start=0, stop=maxsize)` window + identity-first
    // comparison semantics as `list.index`.
    let len = t.len() as i64;
    let adjust = |v: i64| -> i64 {
        if v < 0 {
            (v + len).max(0)
        } else {
            v.min(len)
        }
    };
    let start = match args.get(2) {
        Some(o) => adjust(seq_index_bound(o)?),
        None => 0,
    };
    let stop = match args.get(3) {
        Some(o) => adjust(seq_index_bound(o)?),
        None => len,
    };
    let mut i = start;
    while i < stop {
        if crate::object::member_eq(&t[i as usize], &args[1])? {
            return Ok(Object::Int(i));
        }
        i += 1;
    }
    Err(value_error("tuple.index(x): x not in tuple"))
}

// ---------- dict extras ----------

fn dict_setdefault(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = match args.get(1) {
        Some(k) => DictKey(k.clone()),
        None => return Err(type_error("setdefault() takes at least 1 argument")),
    };
    ensure_hashable(&key.0)?;
    let default = args.get(2).cloned().unwrap_or(Object::None);
    // One probe, one `__hash__` call (CPython's `dict_setdefault` is a
    // single lookup; `test_setdefault_atomic` counts the dispatches).
    if crate::object::dict_key_is_reentrant(&key.0) {
        return crate::object::dict_reentrant_setdefault(&d, key.0, default);
    }
    let (out, deferred) = crate::object::with_key_eq_deferred(|| {
        crate::object::key_cmp_scope(|| {
            let mut borrowed = d.borrow_mut();
            if let Some(v) = borrowed.get(&key).cloned() {
                return (v, true);
            }
            borrowed.insert(key.clone(), default.clone());
            (default.clone(), false)
        })
    });
    let (value, existed) = out?;
    if deferred && !existed {
        // The native probe was inconclusive (a stored key needed a Python
        // comparison) and appended a new entry; undo it and redo the whole
        // operation on the borrow-free path.
        {
            let mut m = d.borrow_mut();
            if m.keys().next_back().is_some_and(|k| k.0.is_same(&key.0)) {
                m.pop();
            }
        }
        return crate::object::dict_reentrant_setdefault(&d, key.0, default);
    }
    if !existed {
        crate::object::dict_watch_bump(&d);
        crate::object::dict_mutation_event(&d);
        if crate::capi_watchers::dicts_active() {
            crate::capi_watchers::dict_event("ADDED", &d, Some(&key.0), Some(&value));
        }
    }
    Ok(value)
}

fn dict_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "copy")?;
    let cloned = d.borrow().clone();
    let out = Object::Dict(Rc::new(RefCell::new(cloned)));
    // CPython's `PyDict_Copy` preserves GC tracking: the copy is tracked
    // iff the source is (test_dict `test_copy_maintains_tracking`).
    if crate::gc_trace::is_tracked(crate::weakref_registry::id_of(&Object::Dict(d))) {
        crate::gc_trace::track(out.clone());
    }
    Ok(out)
}

fn dict_fromkeys(args: &[Object]) -> Result<Object, RuntimeError> {
    // ``fromkeys`` is a *classmethod*: `cls` is the only receiver it ever
    // binds, so the keys always come from the first ordinary argument —
    // never from a "bound dict". Accessing it through an instance/class
    // (`{}.fromkeys(it, val)`, `D.fromkeys(it, val)`) prepends the class, so
    // a leading `Type` shifts the iterable to slot 1; accessing it through
    // the builtin `dict` type itself (`dict.fromkeys(it, val)`) passes no
    // class, leaving the iterable in slot 0. Critically, the iterable may
    // *be* a dict (`dict.fromkeys(other_dict, value)` — bpo do_not_rehash),
    // so we must not mistake a dict in slot 0 for a bound receiver.
    let (cls, it_idx) = match args.first() {
        Some(Object::Type(t)) => (Some(t.clone()), 1usize),
        _ => (None, 0usize),
    };
    let it = args
        .get(it_idx)
        .ok_or_else(|| type_error("fromkeys expected at least 1 argument, got 0"))?;
    let value = args.get(it_idx + 1).cloned().unwrap_or(Object::None);
    let bt = crate::builtin_types::builtin_types();
    let plain = cls.as_ref().is_none_or(|t| Rc::ptr_eq(t, &bt.dict_));
    let interp_ptr = crate::vm_singletons::current_interpreter_ptr();
    if plain {
        // Exact `dict`: build the payload directly. Iterate through the
        // interpreter when one is live so a user iterator that *raises*
        // (test_dict `BadSeq`) propagates instead of reading as exhaustion.
        let d = Rc::new(RefCell::new(DictData::default()));
        if let Some(ptr) = interp_ptr {
            // SAFETY: published by an enclosing VM frame still live on this
            // thread; the GIL keeps it exclusive.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let iter = interp.make_iter(it, &globals)?;
            while let Some(k) = interp.iter_next(&iter, &globals)? {
                ensure_hashable(&k)?;
                dict_insert(&d, k, value.clone())?;
            }
        } else {
            let mut iter = it.make_iter()?;
            while let Some(k) = iter.next_value_checked()? {
                ensure_hashable(&k)?;
                dict_insert(&d, k, value.clone())?;
            }
        }
        return Ok(Object::Dict(d));
    }
    // A dict subclass: CPython's `dict_fromkeys` calls `cls()` — running
    // `__new__`/`__init__`, either of which may raise or return a foreign
    // object like `UserDict` — then `PyObject_SetItem` per key, honouring a
    // user `__setitem__` override (test_dict `baddict1/2`, `mydict`).
    let cls = cls.expect("subclass path requires a class");
    let ptr = interp_ptr
        .ok_or_else(|| type_error("fromkeys(): no interpreter available for dict subclass"))?;
    // SAFETY: as above.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    let target = interp.call_object_with_globals(&Object::Type(cls), &[], &[], &globals)?;
    let iter = interp.make_iter(it, &globals)?;
    while let Some(k) = interp.iter_next(&iter, &globals)? {
        interp.subscr_set_public(&target, &k, value.clone())?;
    }
    Ok(target)
}

/// The dict payload of `other` when it is a dict (or dict-backed
/// subclass instance); `None` for anything else.
fn dict_payload(other: &Object) -> Option<Rc<RefCell<DictData>>> {
    match other {
        Object::Dict(o) => Some(o.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::Dict(o)) => Some(o.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// PEP 584 `d | other`: a new dict; `NotImplemented` for a non-dict RHS.
fn dict_or(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let Some(other) = args.get(1).and_then(dict_payload) else {
        return Ok(crate::vm_singletons::not_implemented());
    };
    let mut out = d.borrow().clone();
    for (k, v) in other.borrow().iter() {
        out.insert(k.clone(), v.clone());
    }
    let obj = Object::Dict(Rc::new(RefCell::new(out)));
    crate::gc_trace::track(obj.clone());
    Ok(obj)
}

/// Reflected PEP 584 merge: `other | d` with `d` supplying the overrides.
fn dict_ror(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let Some(other) = args.get(1).and_then(dict_payload) else {
        return Ok(crate::vm_singletons::not_implemented());
    };
    let mut out = other.borrow().clone();
    for (k, v) in d.borrow().iter() {
        out.insert(k.clone(), v.clone());
    }
    let obj = Object::Dict(Rc::new(RefCell::new(out)));
    crate::gc_trace::track(obj.clone());
    Ok(obj)
}

/// PEP 584 `d |= other` (in place): unlike binary `|`, accepts anything
/// `dict.update` does — a mapping, or an iterable of key/value pairs.
fn dict_ior(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("__ior__ expected 1 argument"))?;
    if let Some(src) = dict_payload(other) {
        let entries: Vec<(DictKey, Object)> = src
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in entries {
            if let Some(old) = dict_insert(&d, k.0, v)? {
                queue_removed(old);
            }
        }
        return Ok(args[0].clone());
    }
    // A mapping instance (`keys()` + `__getitem__`, e.g. `UserDict`)
    // merges key→value like `dict.update` — through the interpreter, since
    // its mapping API is Python code (test_userdict test_mixed_ior).
    if matches!(other, Object::Instance(_)) && crate::instance_method(other, "keys").is_some() {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame on this thread;
            // the GIL keeps it exclusive.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            interp.dict_merge_from(&args[0], other, &globals)?;
            return Ok(args[0].clone());
        }
    }
    // Iterable of pairs (CPython `PyDict_MergeFromSeq2`): a non-iterable
    // RHS is a TypeError, a wrong-length element a ValueError.
    let mut it = other.make_iter().map_err(|_| {
        type_error(format!(
            "unsupported operand type(s) for |=: 'dict' and '{}'",
            other.type_name()
        ))
    })?;
    let mut i = 0usize;
    while let Some(pair) = it.next_value() {
        let mut inner = pair.make_iter().map_err(|_| {
            type_error(format!(
                "cannot convert dictionary update sequence element #{i} to a sequence"
            ))
        })?;
        let mut kv = Vec::with_capacity(2);
        while let Some(v) = inner.next_value() {
            kv.push(v);
            if kv.len() > 2 {
                break;
            }
        }
        if kv.len() != 2 {
            return Err(value_error(format!(
                "dictionary update sequence element #{i} has length {}; 2 is required",
                kv.len()
            )));
        }
        let mut kv = kv.into_iter();
        let (k, v) = (kv.next().unwrap(), kv.next().unwrap());
        ensure_hashable(&k)?;
        if let Some(old) = dict_insert(&d, k, v)? {
            queue_removed(old);
        }
        i += 1;
    }
    Ok(args[0].clone())
}

fn dict_popitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    dict_view_no_args(args, "popitem")?;
    let popped = d.borrow_mut().pop();
    if let Some((k, v)) = popped {
        crate::object::dict_watch_bump(&d);
        crate::object::dict_mutation_event(&d);
        if crate::capi_watchers::dicts_active() {
            crate::capi_watchers::dict_event("DELETED", &d, Some(&k.0), None);
        }
        Ok(Object::new_tuple(vec![k.0, v]))
    } else {
        Err(key_error("popitem(): dictionary is empty"))
    }
}

// ---------- set methods ----------

fn set_self(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("expected set receiver"))
}

/// A single-element in-place set mutation (`add`/`discard`/`clear`).
enum SetInplaceOp {
    Add(DictKey),
    Discard(DictKey),
    Clear,
}

fn apply_set_inplace_op(s: &mut crate::object::SetData, op: SetInplaceOp) {
    match op {
        SetInplaceOp::Add(k) => {
            s.insert(k);
        }
        SetInplaceOp::Discard(k) => {
            if let Some(removed) = s.shift_take(&k) {
                queue_removed(removed.0);
            }
        }
        SetInplaceOp::Clear => {
            for k in s.drain(..) {
                queue_removed(k.0);
            }
        }
    }
}

thread_local! {
    /// In-place set mutations that a *re-entrant* `add`/`discard`/`clear`
    /// (called from inside an element's `__hash__`/`__eq__` while an outer
    /// frame already holds the set's borrow) could not apply immediately.
    ///
    /// CPython tolerates this (e.g. `test_set`'s
    /// `test_hash_collision_concurrent_add`, where a hash-colliding
    /// `__eq__` calls `s.add(...)` during another `s.add`). Rather than
    /// panic on the re-entrant `borrow_mut`, the inner call records its op
    /// here keyed by the set's identity; the outer frame replays it once
    /// its own mutation completes, so the final contents match CPython.
    static SET_DEFERRED_OPS: std::cell::RefCell<Vec<(usize, SetInplaceOp)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain and return any deferred ops queued against `ptr`. The queue
/// borrow is released before the caller applies them, so a further
/// re-entrant mutation during replay can queue again without conflict.
fn take_deferred_set_ops(ptr: usize) -> Vec<SetInplaceOp> {
    SET_DEFERRED_OPS.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            return Vec::new();
        }
        let mut drained = Vec::new();
        let mut i = 0;
        while i < q.len() {
            if q[i].0 == ptr {
                drained.push(q.remove(i).1);
            } else {
                i += 1;
            }
        }
        drained
    })
}

fn set_apply_inplace(args: &[Object], op: SetInplaceOp) -> Result<Object, RuntimeError> {
    match set_self(args)? {
        Object::Set(s) => {
            let ptr = Rc::as_ptr(&s) as usize;
            match s.try_borrow_mut() {
                Ok(mut b) => {
                    // A colliding `__eq__` that raises during `add`/`discard`
                    // aborts the mutation in CPython (test_badcmp `s.add`/
                    // `s.discard`/`s.remove` of a `BadCmp`).
                    crate::object::key_cmp_scope(|| apply_set_inplace_op(&mut b, op))?;
                    // Replay anything a re-entrant callback deferred while we
                    // held the borrow. Looping handles nested re-entrancy.
                    loop {
                        let replay = take_deferred_set_ops(ptr);
                        if replay.is_empty() {
                            break;
                        }
                        for op in replay {
                            apply_set_inplace_op(&mut b, op);
                        }
                    }
                    Ok(Object::None)
                }
                Err(_) => {
                    // An outer frame holds the borrow (re-entrant mutation
                    // from a `__hash__`/`__eq__` callback). Defer; the outer
                    // frame replays once it finishes.
                    SET_DEFERRED_OPS.with(|q| q.borrow_mut().push((ptr, op)));
                    Ok(Object::None)
                }
            }
        }
        Object::FrozenSet(_) => Err(type_error("frozenset is immutable")),
        _ => Err(type_error("expected set receiver")),
    }
}

fn set_iter_items(obj: &Object) -> Result<Vec<DictKey>, RuntimeError> {
    match obj {
        Object::Set(s) => Ok(s.borrow().iter().cloned().collect()),
        Object::FrozenSet(s) => Ok(s.iter().cloned().collect()),
        other => {
            // An arbitrary iterable feeding a set operation
            // (`union`/`update`/`difference`/…): each element becomes a set
            // key, so an unhashable one (`s.union([[]])`) raises `TypeError`
            // exactly like CPython, instead of being silently bucketed.
            let mut it = other.make_iter()?;
            let mut out = Vec::new();
            while let Some(v) = it.next_value() {
                out.push(set_insert_key(&v)?);
            }
            Ok(out)
        }
    }
}

fn set_add(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("add() expected 1 arg"))?;
    // `set.add` *inserts*: an unhashable element (incl. a `set`) raises.
    set_apply_inplace(args, SetInplaceOp::Add(set_insert_key(&v)?))
}

fn set_discard(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("discard() expected 1 arg"))?;
    // `set.discard` is a membership op: a `set` operand looks up its
    // frozenset equivalent; a `list`/etc. still raises TypeError.
    set_apply_inplace(args, SetInplaceOp::Discard(set_membership_key(&v)?))
}

fn set_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("remove() expected 1 arg"))?;
    let key = set_membership_key(&v)?;
    match set_self(args)? {
        Object::Set(s) => {
            let removed = crate::object::key_cmp_scope(|| s.borrow_mut().shift_remove(&key))?;
            if removed {
                Ok(Object::None)
            } else {
                // CPython's `set.remove` raises `KeyError(key)` carrying the
                // *original* operand object (so `e.args[0] is key`), not the
                // frozenset used for the lookup nor its repr string.
                Err(key_error_object(v))
            }
        }
        Object::FrozenSet(_) => Err(type_error("frozenset is immutable")),
        _ => Err(type_error("expected set")),
    }
}

fn set_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    match set_self(args)? {
        Object::Set(s) => {
            let key = s.borrow().iter().next().cloned();
            match key {
                Some(k) => {
                    s.borrow_mut().shift_remove(&k);
                    Ok(k.0)
                }
                None => Err(key_error("pop from an empty set")),
            }
        }
        _ => Err(type_error("expected set")),
    }
}

fn set_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    set_apply_inplace(args, SetInplaceOp::Clear)
}

fn set_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    match set_self(args)? {
        Object::Set(s) => {
            let data: crate::object::SetData = s.borrow().clone();
            Ok(Object::Set(Rc::new(RefCell::new(data))))
        }
        Object::FrozenSet(s) => Ok(Object::FrozenSet(s.clone())),
        _ => Err(type_error("expected set")),
    }
}

fn set_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let receiver = set_self(args)?;
    if let Object::FrozenSet(_) = receiver {
        return Err(type_error("frozenset is immutable"));
    }
    if let Object::Set(s) = &receiver {
        // Build on a snapshot so an element `__hash__` that re-enters and
        // mutates `s` can't fire while we hold `s.borrow_mut()` (bpo-46615).
        let mut acc: crate::object::SetData = s.borrow().clone();
        for other in args.iter().skip(1) {
            for k in set_iter_items(other)? {
                // A colliding `__eq__` raising during the merge aborts the
                // update (test_8420_set_merge: `bad_eq.__eq__` raises
                // ZeroDivisionError).
                crate::object::key_cmp_scope(|| acc.insert(k))?;
            }
        }
        *s.borrow_mut() = acc;
    }
    Ok(Object::None)
}

/// Wrap a computed set body in the storage kind of the *receiver*: CPython's
/// `frozenset.union`/`intersection`/`difference`/`symmetric_difference`
/// return a `frozenset`, while `set`'s return a `set`. Subclass receivers
/// produce the base type (`set`/`frozenset`), matching CPython.
fn set_result_like(receiver: Option<&Object>, body: crate::object::SetData) -> Object {
    let frozen = match receiver {
        Some(Object::FrozenSet(_)) => true,
        Some(Object::Instance(inst)) => {
            matches!(inst.native.get().cloned(), Some(Object::FrozenSet(_)))
        }
        _ => false,
    };
    if frozen {
        Object::FrozenSet(Rc::new(crate::object::FrozenSetObj::new(body)))
    } else {
        Object::Set(Rc::new(RefCell::new(body)))
    }
}

fn set_union(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out = crate::object::SetData::default();
    if let Some(first) = args.first() {
        for k in set_iter_items(first)? {
            out.insert(k);
        }
    }
    for other in args.iter().skip(1) {
        for k in set_iter_items(other)? {
            out.insert(k);
        }
    }
    Ok(set_result_like(args.first(), out))
}

fn set_intersection(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut acc = match args.first() {
        Some(first) => {
            let mut s = crate::object::SetData::default();
            for k in set_iter_items(first)? {
                s.insert(k);
            }
            s
        }
        None => return Ok(Object::new_set()),
    };
    for other in args.iter().skip(1) {
        let other_set: crate::object::SetData = set_iter_items(other)?.into_iter().collect();
        acc.retain(|k| other_set.contains(k));
    }
    Ok(set_result_like(args.first(), acc))
}

fn set_difference(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut acc = match args.first() {
        Some(first) => {
            let mut s = crate::object::SetData::default();
            for k in set_iter_items(first)? {
                s.insert(k);
            }
            s
        }
        None => return Ok(Object::new_set()),
    };
    for other in args.iter().skip(1) {
        let other_set: crate::object::SetData = set_iter_items(other)?.into_iter().collect();
        acc.retain(|k| !other_set.contains(k));
    }
    Ok(set_result_like(args.first(), acc))
}

fn set_symmetric_difference(args: &[Object]) -> Result<Object, RuntimeError> {
    let a: crate::object::SetData = match args.first() {
        Some(first) => set_iter_items(first)?.into_iter().collect(),
        None => return Ok(Object::new_set()),
    };
    let b: crate::object::SetData = match args.get(1) {
        Some(other) => set_iter_items(other)?.into_iter().collect(),
        None => return Ok(set_result_like(args.first(), a)),
    };
    let mut out = crate::object::SetData::default();
    for k in a.iter().filter(|k| !b.contains(*k)) {
        out.insert(k.clone());
    }
    for k in b.iter().filter(|k| !a.contains(*k)) {
        out.insert(k.clone());
    }
    Ok(set_result_like(args.first(), out))
}

fn set_issubset(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = set_iter_items(args.first().unwrap())?;
    let b: crate::object::SetData = match args.get(1) {
        Some(o) => set_iter_items(o)?.into_iter().collect(),
        None => return Err(type_error("issubset() expected 1 arg")),
    };
    Ok(Object::Bool(a.iter().all(|k| b.contains(k))))
}

fn set_issuperset(args: &[Object]) -> Result<Object, RuntimeError> {
    let a: crate::object::SetData = set_iter_items(args.first().unwrap())?.into_iter().collect();
    let b = match args.get(1) {
        Some(o) => set_iter_items(o)?,
        None => return Err(type_error("issuperset() expected 1 arg")),
    };
    Ok(Object::Bool(b.iter().all(|k| a.contains(k))))
}

fn set_isdisjoint(args: &[Object]) -> Result<Object, RuntimeError> {
    let a: crate::object::SetData = set_iter_items(args.first().unwrap())?.into_iter().collect();
    let b = match args.get(1) {
        Some(o) => set_iter_items(o)?,
        None => return Err(type_error("isdisjoint() expected 1 arg")),
    };
    Ok(Object::Bool(!b.iter().any(|k| a.contains(k))))
}

fn set_intersection_update(args: &[Object]) -> Result<Object, RuntimeError> {
    if matches!(set_self(args)?, Object::FrozenSet(_)) {
        return Err(type_error("frozenset is immutable"));
    }
    if let Object::Set(s) = set_self(args)? {
        let mut keep: crate::object::SetData = s.borrow().clone();
        for other in args.iter().skip(1) {
            let o: crate::object::SetData = set_iter_items(other)?.into_iter().collect();
            keep.retain(|k| o.contains(k));
        }
        *s.borrow_mut() = keep;
    }
    Ok(Object::None)
}

fn set_difference_update(args: &[Object]) -> Result<Object, RuntimeError> {
    if matches!(set_self(args)?, Object::FrozenSet(_)) {
        return Err(type_error("frozenset is immutable"));
    }
    if let Object::Set(s) = set_self(args)? {
        // Compute on a snapshot so the element `__hash__`/`__eq__` we
        // invoke (which may re-enter and clear `s`) never runs while we
        // hold `s.borrow_mut()` — see the bpo-46615 note in `set_intersection_update`.
        let mut keep: crate::object::SetData = s.borrow().clone();
        for other in args.iter().skip(1) {
            for k in set_iter_items(other)? {
                keep.shift_remove(&k);
            }
        }
        *s.borrow_mut() = keep;
    }
    Ok(Object::None)
}

fn set_symmetric_difference_update(args: &[Object]) -> Result<Object, RuntimeError> {
    if matches!(set_self(args)?, Object::FrozenSet(_)) {
        return Err(type_error("frozenset is immutable"));
    }
    if let Object::Set(s) = set_self(args)? {
        let b: crate::object::SetData = match args.get(1) {
            Some(o) => set_iter_items(o)?.into_iter().collect(),
            None => return Ok(Object::None),
        };
        let a: crate::object::SetData = s.borrow().clone();
        let mut out = crate::object::SetData::default();
        for k in a.iter().filter(|k| !b.contains(*k)) {
            out.insert(k.clone());
        }
        for k in b.iter().filter(|k| !a.contains(*k)) {
            out.insert(k.clone());
        }
        *s.borrow_mut() = out;
    }
    Ok(Object::None)
}

// ---------- bytes methods ----------

fn bytes_data(args: &[Object]) -> Result<Vec<u8>, RuntimeError> {
    match args.first() {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        _ => Err(type_error("expected bytes-like receiver")),
    }
}

/// Run `f` over the receiver's bytes *without* copying them. The search
/// family (`find`/`rfind`/`count`/`index`/`rindex`) is called in tight
/// loops over megabyte haystacks (test_bytes' fastsearch suite); a
/// per-call `to_vec` of the haystack turns the O(n+m) search into an
/// O(n) copy per call and blows the suite's time budget.
fn with_bytes_data<R>(
    args: &[Object],
    f: impl FnOnce(&[u8]) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    match args.first() {
        Some(Object::Bytes(b)) => f(b),
        Some(Object::ByteArray(b)) => {
            let guard = b.borrow();
            f(&guard)
        }
        _ => Err(type_error("expected bytes-like receiver")),
    }
}

pub(crate) fn bytes_argview(arg: &Object) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::MemoryView(mv) => Ok(mv.to_bytes()),
        Object::Instance(inst) => {
            // bytes/bytearray subclasses carry their payload natively.
            if let Some(native) = inst.native.get() {
                let native = native.clone();
                if matches!(
                    native,
                    Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
                ) {
                    return bytes_argview(&native);
                }
            }
            // PEP 688: an object exposing `__buffer__` works anywhere a
            // bytes-like object is accepted. Reenter the interpreter to
            // call it (CPython's PyObject_GetBuffer slot dispatch).
            if let Some(method) = crate::instance_method(arg, "__buffer__") {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still live
                    // on this thread; the GIL keeps the access exclusive.
                    let interp = unsafe { &mut *ptr };
                    let globals = interp.builtins_dict();
                    let r = interp.call_object_with_globals(
                        &method,
                        &[Object::Int(0)],
                        &[],
                        &globals,
                    )?;
                    return match &r {
                        Object::MemoryView(mv) => Ok(mv.to_bytes()),
                        Object::Bytes(b) => Ok(b.to_vec()),
                        Object::ByteArray(b) => Ok(b.borrow().clone()),
                        _ => Err(type_error(format!(
                            "__buffer__ returned non-buffer of type '{}'",
                            r.type_name()
                        ))),
                    };
                }
            }
            Err(type_error(format!(
                "a bytes-like object is required, not '{}'",
                arg.type_name()
            )))
        }
        _ => Err(type_error(format!(
            "a bytes-like object is required, not '{}'",
            arg.type_name()
        ))),
    }
}

/// Needle argument of `bytes.find` / `rfind` / `index` / `rindex` /
/// `count` / `in`: a bytes-like object, or an integer naming a single
/// byte (range-checked like CPython's `_getbytevalue`). Objects with a
/// user `__index__` go through interpreter reentry like CPython's
/// `PyNumber_Index` path.
fn bytes_find_needle(arg: &Object) -> Result<Vec<u8>, RuntimeError> {
    let native = arg.native_value();
    match native.as_ref().unwrap_or(arg) {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::MemoryView(mv) => Ok(mv.to_bytes()),
        Object::Bool(v) => Ok(vec![u8::from(*v)]),
        Object::Int(i) => {
            if (0..=255).contains(i) {
                Ok(vec![*i as u8])
            } else {
                Err(value_error("byte must be in range(0, 256)"))
            }
        }
        Object::Long(_) => Err(value_error("byte must be in range(0, 256)")),
        inst @ Object::Instance(_) if crate::instance_method(inst, "__index__").is_some() => {
            let v = coerce_index_i64(inst)?;
            if (0..=255).contains(&v) {
                Ok(vec![v as u8])
            } else {
                Err(value_error("byte must be in range(0, 256)"))
            }
        }
        _ => Err(type_error(format!(
            "argument should be integer or bytes-like object, not '{}'",
            arg.type_name()
        ))),
    }
}

/// Build a transform result that follows the receiver's type
/// (`bytes.lower() -> bytes`, `bytearray.lower() -> bytearray`).
fn bytes_like_result(args: &[Object], out: Vec<u8>) -> Object {
    if matches!(args.first(), Some(Object::ByteArray(_))) {
        Object::new_bytearray(out)
    } else {
        Object::new_bytes(out)
    }
}

fn byte_is_pyspace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
}

fn bytes_decode_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let encoding = match arg_or_kw(args, 1, kwargs, "encoding") {
        Some(Object::Str(e)) => e.to_string(),
        // A lone-surrogate-bearing name is still a *str* to CPython — the
        // codec lookup simply fails, and callers catch the LookupError
        // (email._encoded_words.decode with a mangled RFC 2047 charset —
        // test_email test_invalid_character_in_charset).
        Some(Object::WStr(w)) => {
            return Err(crate::error::lookup_error(format!(
                "unknown encoding: {}",
                Object::WStr(w.clone()).to_str()
            )))
        }
        None => "utf-8".to_owned(),
        _ => return Err(type_error("decode() expected str")),
    };
    let errors = match arg_or_kw(args, 2, kwargs, "errors") {
        Some(Object::Str(e)) => e.to_string(),
        _ => "strict".to_owned(),
    };
    // Produces a surrogate-bearing `WStr` for `surrogateescape`/`surrogatepass`.
    crate::stdlib::codecs_mod::decode_bytes_obj(&data, &encoding, &errors)
}

/// Call an instance's PEP 688 `__buffer__` and hand back the exported
/// memoryview (`None` when the object doesn't export one, or the export
/// fails). Used for structural buffer comparisons.
/// Whether `obj` is a VM instance whose class implements the PEP 688
/// buffer protocol (`__buffer__`). Used by the C-API bridge's
/// `PyObject_CheckBuffer` without actually invoking the exporter.
pub fn has_buffer_dunder(obj: &Object) -> bool {
    crate::instance_method(obj, "__buffer__").is_some()
}

pub fn buffer_exported_view(obj: &Object) -> Option<Rc<crate::object::PyMemoryView>> {
    let method = crate::instance_method(obj, "__buffer__")?;
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    match interp.call_object_with_globals(&method, &[Object::Int(0)], &[], &globals) {
        Ok(Object::MemoryView(mv)) => Some(mv),
        _ => None,
    }
}

fn bytes_hex_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = match args.first() {
        Some(Object::MemoryView(mv)) => mv.to_bytes(),
        _ => bytes_data(args)?,
    };
    let mut sep_obj = args.get(1).cloned();
    let mut bps_obj = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "sep" => sep_obj = Some(v.clone()),
            "bytes_per_sep" => bps_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "hex() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // CPython sizes `sep` with `PyObject_Length`, which dispatches an
    // overridden `__len__` — user code that may try to resize the
    // receiving bytearray. Hold an export so that resize raises
    // BufferError instead of leaving the hex loop reading freed
    // memory (gh-143195).
    let _guard = bytearray_receiver_guard(args);
    let sep: Option<u8> = match &sep_obj {
        None => None,
        Some(sep_arg) => {
            // Virtual length first (an overridden `__len__` runs here);
            // then validate against the native payload.
            let native = sep_arg.native_value();
            let unwrapped = native.as_ref().unwrap_or(sep_arg);
            let reported_len: Option<i64> = if let (Object::Instance(_), Some(m)) =
                (sep_arg, crate::instance_method(sep_arg, "__len__"))
            {
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by an enclosing VM frame still
                    // live on this thread; the GIL keeps it exclusive.
                    let interp = unsafe { &mut *ptr };
                    let globals = interp.builtins_dict();
                    let r = interp.call_object_with_globals(&m, &[], &[], &globals)?;
                    r.as_i64()
                } else {
                    None
                }
            } else {
                None
            };
            match unwrapped {
                Object::Str(s) => {
                    let n = reported_len.unwrap_or_else(|| s.chars().count() as i64);
                    if n != 1 {
                        return Err(value_error("sep must be length 1."));
                    }
                    let c = s
                        .chars()
                        .next()
                        .ok_or_else(|| value_error("sep must be length 1."))?;
                    if (c as u32) > 0x7f {
                        return Err(value_error("sep must be ASCII."));
                    }
                    Some(c as u8)
                }
                Object::Bytes(b) => {
                    let n = reported_len.unwrap_or(b.len() as i64);
                    if n != 1 || b.is_empty() {
                        return Err(value_error("sep must be length 1."));
                    }
                    if b[0] > 0x7f {
                        return Err(value_error("sep must be ASCII."));
                    }
                    Some(b[0])
                }
                Object::ByteArray(b) => {
                    let b = b.borrow();
                    let n = reported_len.unwrap_or(b.len() as i64);
                    if n != 1 || b.is_empty() {
                        return Err(value_error("sep must be length 1."));
                    }
                    if b[0] > 0x7f {
                        return Err(value_error("sep must be ASCII."));
                    }
                    Some(b[0])
                }
                other => {
                    return Err(type_error(format!(
                        "sep must be str or bytes, not {}",
                        other.type_name()
                    )))
                }
            }
        }
    };
    let bytes_per_sep = match &bps_obj {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(_)) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C int",
            ))
        }
        None => 1,
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )))
        }
    };
    let mut out = String::with_capacity(data.len() * 2);
    let step = bytes_per_sep.unsigned_abs() as usize;
    for (i, b) in data.iter().enumerate() {
        if let Some(sep) = sep {
            if i > 0 && step > 0 {
                // CPython 3.13: positive ``bytes_per_sep`` groups
                // bytes from the right; negative groups from the
                // left. The separator falls BEFORE the byte at
                // ``i`` when the remaining or leading run lines up
                // on a group boundary.
                let needs_sep = if bytes_per_sep < 0 {
                    i % step == 0
                } else {
                    (data.len() - i) % step == 0
                };
                if needs_sep {
                    out.push(sep as char);
                }
            }
        }
        out.push_str(&format!("{b:02x}"));
    }
    Ok(Object::from_str(out))
}

fn bytes_fromhex(args: &[Object]) -> Result<Object, RuntimeError> {
    // First arg is `cls` for classmethod-style. Fish out the string.
    let s_obj = if matches!(
        args.first(),
        Some(Object::Type(_)) | Some(Object::Bytes(_)) | Some(Object::ByteArray(_))
    ) {
        args.get(1)
    } else {
        args.first()
    };
    let s = fromhex_string_arg(s_obj)?;
    let bytes = parse_hex_bytes(&s)?;
    // Decide return type based on receiver: bytearray.fromhex returns bytearray;
    // bytes.fromhex returns bytes.
    if matches!(args.first(), Some(Object::ByteArray(_))) {
        Ok(Object::new_bytearray(bytes))
    } else {
        Ok(Object::new_bytes(bytes))
    }
}

fn bytes_startswith(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("startswith", args)?;
    let target = args
        .get(1)
        .ok_or_else(|| type_error("startswith() expected 1 arg"))?;
    with_bytes_data(args, |data| {
        let (start, end, invalid) = bytes_search_range(args, data.len());
        if invalid {
            return Ok(Object::Bool(false));
        }
        Ok(Object::Bool(bytes_match_prefix_suffix(
            &data[start..end],
            target,
            true,
        )?))
    })
}

fn bytes_endswith(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("endswith", args)?;
    let target = args
        .get(1)
        .ok_or_else(|| type_error("endswith() expected 1 arg"))?;
    with_bytes_data(args, |data| {
        let (start, end, invalid) = bytes_search_range(args, data.len());
        if invalid {
            return Ok(Object::Bool(false));
        }
        Ok(Object::Bool(bytes_match_prefix_suffix(
            &data[start..end],
            target,
            false,
        )?))
    })
}

fn bytes_match_prefix_suffix(
    data: &[u8],
    target: &Object,
    prefix: bool,
) -> Result<bool, RuntimeError> {
    let name = if prefix { "startswith" } else { "endswith" };
    let test = |needle: &[u8]| {
        if prefix {
            data.starts_with(needle)
        } else {
            data.ends_with(needle)
        }
    };
    match target {
        Object::Tuple(parts) => {
            for item in parts.iter() {
                let needle = bytes_argview(item).map_err(|_| {
                    type_error(format!(
                        "tuple for {name} must only contain bytes-like objects, \
                         not '{}'",
                        item.type_name()
                    ))
                })?;
                if test(&needle) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => {
            let needle = bytes_argview(target).map_err(|_| {
                type_error(format!(
                    "{name} first arg must be bytes or a tuple of bytes, not {}",
                    target.type_name()
                ))
            })?;
            Ok(test(&needle))
        }
    }
}

/// Resolve the optional `start`/`end` arguments of `bytes.find` and
/// friends (positions 2 and 3) the way CPython's `ADJUST_INDICES`
/// does: negative indices are offset by the length and floored at 0,
/// `end` is clamped to the length but `start` is **not** — a start
/// past the end makes the window invalid (third tuple slot), which
/// matters for empty needles (`b'abc'.find(b'', 4) == -1`).
fn bytes_search_range(args: &[Object], len: usize) -> (usize, usize, bool) {
    let n = len as i64;
    let resolve = |o: Option<&Object>, default: i64| -> i64 {
        match o {
            None | Some(Object::None) => default,
            Some(obj) => match obj.as_i64() {
                Some(x) => {
                    if x < 0 {
                        (x + n).max(0)
                    } else {
                        x
                    }
                }
                None => default,
            },
        }
    };
    let raw_start = resolve(args.get(2), 0);
    let end = resolve(args.get(3), n).clamp(0, n);
    let invalid = raw_start > end;
    let start = raw_start.clamp(0, end.max(0));
    (start as usize, end as usize, invalid)
}

/// Find `sub` within `data[start..end]`, returning the *absolute*
/// position (or -1). Mirrors `bytes.find`'s empty-needle behaviour.
/// `memmem` is O(n + m) like CPython's stringlib fastsearch — the
/// suite checks this (`test_adaptive_find` with megabyte needles).
fn bytes_find_in(data: &[u8], sub: &[u8], start: usize, end: usize) -> i64 {
    if start > end || end > data.len() {
        return -1;
    }
    let hay = &data[start..end];
    if sub.is_empty() {
        return start as i64;
    }
    memchr::memmem::find(hay, sub).map_or(-1, |i| (start + i) as i64)
}

/// gh-142560: converting a search argument can run Python code (a user
/// `__index__` / `__buffer__`) that tries to resize the receiving
/// bytearray while the search holds its buffer. Holding a real export
/// for the duration makes the offending resize raise `BufferError`
/// at the mutation site, exactly like CPython.
fn bytes_needle_guarded(args: &[Object], arg: &Object) -> Result<Vec<u8>, RuntimeError> {
    let _guard = bytearray_receiver_guard(args);
    bytes_find_needle(arg)
}

/// Export the receiver's buffer (when it is a bytearray) for the
/// lifetime of the returned guard.
pub(crate) fn bytearray_receiver_guard(
    args: &[Object],
) -> Option<crate::object::ByteArrayExportGuard> {
    match args.first() {
        Some(Object::ByteArray(cell)) => {
            Some(crate::object::ByteArrayExportGuard::new(cell.clone()))
        }
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::ByteArray(cell)) => {
                Some(crate::object::ByteArrayExportGuard::new(cell.clone()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// `_PyArg_CheckPositional("find", nargs, 1, 3)` — the search family
/// takes `sub[, start[, end]]`.
fn bytes_search_arity(name: &str, args: &[Object]) -> Result<(), RuntimeError> {
    let nargs = args.len().saturating_sub(1);
    if nargs > 3 {
        return Err(type_error(format!(
            "{name} expected at most 3 arguments, got {nargs}"
        )));
    }
    if nargs < 1 {
        return Err(type_error(format!(
            "{name} expected at least 1 argument, got {nargs}"
        )));
    }
    Ok(())
}

fn bytes_find(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("find", args)?;
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("find() expected 1 arg"))?,
    )?;
    with_bytes_data(args, |data| {
        let (start, end, invalid) = bytes_search_range(args, data.len());
        if invalid {
            return Ok(Object::Int(-1));
        }
        Ok(Object::Int(bytes_find_in(data, &sub, start, end)))
    })
}

fn bytes_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("rfind", args)?;
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("rfind() expected 1 arg"))?,
    )?;
    with_bytes_data(args, |data| {
        let (start, end, invalid) = bytes_search_range(args, data.len());
        if invalid || end > data.len() {
            return Ok(Object::Int(-1));
        }
        if sub.is_empty() {
            return Ok(Object::Int(end as i64));
        }
        let last =
            memchr::memmem::rfind(&data[start..end], &sub).map_or(-1, |i| (start + i) as i64);
        Ok(Object::Int(last))
    })
}

fn bytes_index(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("index", args)?;
    match bytes_find(args)? {
        Object::Int(i) if i >= 0 => Ok(Object::Int(i)),
        _ => Err(value_error("subsection not found")),
    }
}

fn bytes_rindex(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("rindex", args)?;
    match bytes_rfind(args)? {
        Object::Int(i) if i >= 0 => Ok(Object::Int(i)),
        _ => Err(value_error("subsection not found")),
    }
}

fn bytes_count(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_search_arity("count", args)?;
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("count() expected 1 arg"))?,
    )?;
    with_bytes_data(args, |data| {
        let (start, end, invalid) = bytes_search_range(args, data.len());
        if invalid {
            return Ok(Object::Int(0));
        }
        if sub.is_empty() {
            return Ok(Object::Int((end - start) as i64 + 1));
        }
        // Non-overlapping occurrences, like CPython's `stringlib_count`.
        let n = memchr::memmem::find_iter(&data[start..end], &sub).count() as i64;
        Ok(Object::Int(n))
    })
}

/// CPython parity: the no-argument bytes/bytearray methods
/// (`upper`, `islower`, …) are `METH_NOARGS` and raise `TypeError`
/// when called with anything beyond the receiver.
fn bytes_no_args(name: &str, args: &[Object]) -> Result<(), RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "{name}() takes no arguments ({} given)",
            args.len() - 1
        )));
    }
    Ok(())
}

fn bytes_lower(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("lower", args)?;
    let out: Vec<u8> = bytes_data(args)?
        .iter()
        .map(|b| b.to_ascii_lowercase())
        .collect();
    Ok(bytes_like_result(args, out))
}

fn bytes_upper(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("upper", args)?;
    let out: Vec<u8> = bytes_data(args)?
        .iter()
        .map(|b| b.to_ascii_uppercase())
        .collect();
    Ok(bytes_like_result(args, out))
}

fn bytes_strip(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let trim_set: Vec<u8> = match args.get(1) {
        None | Some(Object::None) => b" \t\n\r\x0b\x0c".to_vec(),
        Some(other) => bytes_argview(other)?,
    };
    let start = data
        .iter()
        .position(|b| !trim_set.contains(b))
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !trim_set.contains(b))
        .map_or(start, |i| i + 1);
    if let Some(same) = bytes_unchanged_self(args, end - start == data.len()) {
        return Ok(same);
    }
    Ok(bytes_like_result(args, data[start..end].to_vec()))
}

/// CPython's strip-family identity optimization for exact `bytes`:
/// return *self* when nothing was removed (`test_bigmem` asserts
/// `b.lstrip() is b`). `bytearray` always copies.
fn bytes_unchanged_self(args: &[Object], unchanged: bool) -> Option<Object> {
    if unchanged {
        if let Some(recv @ Object::Bytes(_)) = args.first() {
            return Some(recv.clone());
        }
    }
    None
}

fn bytes_lstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let trim_set: Vec<u8> = match args.get(1) {
        None | Some(Object::None) => b" \t\n\r\x0b\x0c".to_vec(),
        Some(other) => bytes_argview(other)?,
    };
    let start = data
        .iter()
        .position(|b| !trim_set.contains(b))
        .unwrap_or(data.len());
    if let Some(same) = bytes_unchanged_self(args, start == 0) {
        return Ok(same);
    }
    Ok(bytes_like_result(args, data[start..].to_vec()))
}

fn bytes_rstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let trim_set: Vec<u8> = match args.get(1) {
        None | Some(Object::None) => b" \t\n\r\x0b\x0c".to_vec(),
        Some(other) => bytes_argview(other)?,
    };
    let end = data
        .iter()
        .rposition(|b| !trim_set.contains(b))
        .map_or(0, |i| i + 1);
    if let Some(same) = bytes_unchanged_self(args, end == data.len()) {
        return Ok(same);
    }
    Ok(bytes_like_result(args, data[..end].to_vec()))
}

/// Shared argument parsing for `bytes.split` / `rsplit`:
/// `(sep=None, maxsplit=-1)`, both passable as keywords.
fn bytes_split_args(
    args: &[Object],
    kwargs: &[(String, Object)],
    name: &str,
) -> Result<(Vec<u8>, Option<Vec<u8>>, i64), RuntimeError> {
    let data = bytes_data(args)?;
    let mut sep_obj = args.get(1).cloned();
    let mut maxsplit_obj = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "sep" => sep_obj = Some(v.clone()),
            "maxsplit" => maxsplit_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "{name}() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let sep = match sep_obj {
        None | Some(Object::None) => None,
        Some(other) => {
            // Same reentrancy hazard as the find family (gh-142560):
            // converting `sep` can run user code (`__buffer__`) that
            // tries to resize the receiving bytearray.
            let _guard = bytearray_receiver_guard(args);
            Some(bytes_argview(&other)?)
        }
    };
    if let Some(s) = &sep {
        if s.is_empty() {
            return Err(value_error("empty separator"));
        }
    }
    let maxsplit = match maxsplit_obj {
        None => -1,
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("integer argument expected"))?,
    };
    Ok((data, sep, maxsplit))
}

fn bytes_split_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (data, sep, maxsplit) = bytes_split_args(args, kwargs, "split")?;
    let mut parts: Vec<Vec<u8>> = Vec::new();
    match sep {
        None => {
            let mut i = 0;
            let mut nsplit = 0i64;
            while i < data.len() {
                while i < data.len() && byte_is_pyspace(data[i]) {
                    i += 1;
                }
                if i >= data.len() {
                    break;
                }
                if maxsplit >= 0 && nsplit >= maxsplit {
                    parts.push(data[i..].to_vec());
                    break;
                }
                let start = i;
                while i < data.len() && !byte_is_pyspace(data[i]) {
                    i += 1;
                }
                parts.push(data[start..i].to_vec());
                nsplit += 1;
            }
        }
        Some(sep) => {
            let mut start = 0;
            let mut nsplit = 0i64;
            while maxsplit < 0 || nsplit < maxsplit {
                match memchr::memmem::find(&data[start..], &sep) {
                    Some(rel) => {
                        parts.push(data[start..start + rel].to_vec());
                        start += rel + sep.len();
                        nsplit += 1;
                    }
                    None => break,
                }
            }
            parts.push(data[start..].to_vec());
        }
    }
    let is_ba = matches!(args.first(), Some(Object::ByteArray(_)));
    Ok(Object::new_list(
        parts
            .into_iter()
            .map(|p| {
                if is_ba {
                    Object::new_bytearray(p)
                } else {
                    Object::new_bytes(p)
                }
            })
            .collect(),
    ))
}

fn bytes_rsplit_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let (data, sep, maxsplit) = bytes_split_args(args, kwargs, "rsplit")?;
    let mut parts: Vec<Vec<u8>> = Vec::new();
    match sep {
        None => {
            let mut i = data.len();
            let mut nsplit = 0i64;
            while i > 0 {
                while i > 0 && byte_is_pyspace(data[i - 1]) {
                    i -= 1;
                }
                if i == 0 {
                    break;
                }
                if maxsplit >= 0 && nsplit >= maxsplit {
                    parts.push(data[..i].to_vec());
                    break;
                }
                let end = i;
                while i > 0 && !byte_is_pyspace(data[i - 1]) {
                    i -= 1;
                }
                parts.push(data[i..end].to_vec());
                nsplit += 1;
            }
            parts.reverse();
        }
        Some(sep) => {
            let mut end = data.len();
            let mut nsplit = 0i64;
            while maxsplit < 0 || nsplit < maxsplit {
                match memchr::memmem::rfind(&data[..end], &sep) {
                    Some(pos) => {
                        parts.push(data[pos + sep.len()..end].to_vec());
                        end = pos;
                        nsplit += 1;
                    }
                    None => break,
                }
            }
            parts.push(data[..end].to_vec());
            parts.reverse();
        }
    }
    let is_ba = matches!(args.first(), Some(Object::ByteArray(_)));
    Ok(Object::new_list(
        parts
            .into_iter()
            .map(|p| {
                if is_ba {
                    Object::new_bytearray(p)
                } else {
                    Object::new_bytes(p)
                }
            })
            .collect(),
    ))
}

fn bytes_splitlines_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    if args.len() > 2 {
        return Err(type_error(format!(
            "splitlines() takes at most 1 argument ({} given)",
            args.len() - 1
        )));
    }
    let mut keepends_obj = args.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "keepends" => keepends_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "splitlines() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let keepends = match &keepends_obj {
        None => false,
        Some(o) => o
            .as_i64()
            .map(|v| v != 0)
            .ok_or_else(|| type_error("an integer is required"))?,
    };
    let mut out: Vec<Object> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\n' || data[i] == b'\r' {
            let no_eol = i;
            let mut end = i + 1;
            if data[i] == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
                end = i + 2;
            }
            let slice = if keepends {
                &data[start..end]
            } else {
                &data[start..no_eol]
            };
            out.push(bytes_like_result(args, slice.to_vec()));
            start = end;
            i = end;
        } else {
            i += 1;
        }
    }
    if start < data.len() {
        out.push(bytes_like_result(args, data[start..].to_vec()));
    }
    Ok(Object::new_list(out))
}

/// `bytes.__mod__` / `bytearray.__mod__` — PEP 461 formatting through
/// the running interpreter (instances may need `__bytes__`/`__repr__`).
fn bytes_dunder_mod(args: &[Object]) -> Result<Object, RuntimeError> {
    let receiver = args
        .first()
        .ok_or_else(|| type_error("__mod__ requires a receiver"))?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("__mod__ expected 1 argument"))?;
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        interp.bytes_percent_format(receiver, other, &globals)
    } else {
        Err(type_error("bytes %-formatting requires the interpreter"))
    }
}

/// `str.__rmod__`: CPython exposes the reflected wrapper of `unicode_mod`
/// on `str` itself (`'__rmod__' in vars(str)` is True). It matters for
/// `str` *subclasses* whose MRO carries a foreign `__rmod__` further up:
/// numpy's `str_` inherits `str.__rmod__` ahead of `generic.__rmod__` (the
/// `remainder` ufunc, which raises on strings), so `"'%s'" % np.str_(…)`
/// must resolve to this wrapper. Formats only when the left operand is a
/// genuine `str`; otherwise `NotImplemented`.
fn str_dunder_rmod(args: &[Object]) -> Result<Object, RuntimeError> {
    let receiver = args
        .first()
        .ok_or_else(|| type_error("__rmod__ requires a receiver"))?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("__rmod__ expected 1 argument"))?;
    if !other.is_str() {
        return Ok(crate::vm_singletons::not_implemented());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by an enclosing VM frame still live on this
        // thread; the GIL keeps the access exclusive.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        interp.percent_mod_left_slot(other, receiver, &globals)
    } else {
        crate::binary_op(other, receiver, weavepy_compiler::BinOpKind::Mod)
    }
}

/// `bytes.__rmod__`: only formats when the *left* operand is bytes-like
/// (then it's really that operand's format), otherwise `NotImplemented`.
fn bytes_dunder_rmod(args: &[Object]) -> Result<Object, RuntimeError> {
    let receiver = args
        .first()
        .ok_or_else(|| type_error("__rmod__ requires a receiver"))?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("__rmod__ expected 1 argument"))?;
    if matches!(other, Object::Bytes(_) | Object::ByteArray(_)) {
        let swapped = [other.clone(), receiver.clone()];
        bytes_dunder_mod(&swapped)
    } else {
        Ok(crate::vm_singletons::not_implemented())
    }
}

fn bytes_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let sep = bytes_data(args)?;
    let it = args
        .get(1)
        .ok_or_else(|| type_error("join() expected iterable"))?;
    // Iterate through the interpreter so user iterables / generators
    // work, not just native containers.
    let items: Vec<Object> = match it {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.to_vec(),
        other => {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame still live on
                // this thread; the GIL keeps the access exclusive.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                interp.collect_iterable(other, &globals)?
            } else {
                let mut iter = other.make_iter()?;
                let mut out = Vec::new();
                while let Some(v) = iter.next_value() {
                    out.push(v);
                }
                out
            }
        }
    };
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(items.len());
    for v in &items {
        let part = bytes_argview(v).map_err(|_| {
            type_error(format!(
                "sequence item {}: expected a bytes-like object, {} found",
                parts.len(),
                v.type_name()
            ))
        })?;
        parts.push(part);
    }
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        out.extend_from_slice(p);
    }
    Ok(bytes_like_result(args, out))
}

fn bytes_replace_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let from = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("replace() expected 2 args"))?,
    )?;
    let to = bytes_argview(
        args.get(2)
            .ok_or_else(|| type_error("replace() expected 2 args"))?,
    )?;
    let mut max_obj = args.get(3).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "count" => max_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "replace() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let max = match max_obj {
        None | Some(Object::None) => -1i64,
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("integer argument expected"))?,
    };
    let mut out = Vec::new();
    let mut done = 0i64;
    let mut i = 0;
    while i < data.len() {
        let within_budget = max < 0 || done < max;
        if within_budget && i + from.len() <= data.len() && data[i..i + from.len()] == from[..] {
            out.extend_from_slice(&to);
            done += 1;
            i += from.len().max(1);
            if from.is_empty() {
                out.push(data[i - 1]);
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    // An empty needle also matches at end-of-string (CPython appends a
    // final replacement: `b"ab".replace(b"", b"-") == b"-a-b-"`).
    if from.is_empty() && (max < 0 || done < max) {
        out.extend_from_slice(&to);
    }
    Ok(bytes_like_result(args, out))
}

/// `bytes.translate(table, /, delete=b'')` and the `bytearray`
/// equivalent. `table` is `None` (identity) or a bytes-like of length
/// 256; bytes present in `delete` are dropped first. The receiver's
/// type (bytes vs bytearray) is preserved.
fn bytes_translate_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let mut delete_obj = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "delete" => delete_obj = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "translate() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let table = match args.get(1) {
        None => {
            return Err(type_error(
                "translate() takes at least 1 argument (0 given)",
            ))
        }
        Some(Object::None) => None,
        Some(o) => {
            let t = bytes_argview(o)?;
            if t.len() != 256 {
                return Err(value_error("translation table must be 256 characters long"));
            }
            Some(t)
        }
    };
    let delete = match delete_obj {
        None => Vec::new(),
        Some(o) => bytes_argview(&o)?,
    };
    let mut out = Vec::with_capacity(data.len());
    for &b in &data {
        if delete.contains(&b) {
            continue;
        }
        out.push(match &table {
            Some(t) => t[b as usize],
            None => b,
        });
    }
    if matches!(args.first(), Some(Object::ByteArray(_))) {
        Ok(Object::new_bytearray(out))
    } else {
        Ok(Object::new_bytes(out))
    }
}

/// `bytes.maketrans(from, to)` — builds a 256-byte translation table
/// mapping each byte in `from` to the byte at the same index in `to`.
fn bytes_maketrans(args: &[Object]) -> Result<Object, RuntimeError> {
    let from = bytes_argview(
        args.first()
            .ok_or_else(|| type_error("maketrans() takes exactly two arguments"))?,
    )?;
    let to = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("maketrans() takes exactly two arguments"))?,
    )?;
    if from.len() != to.len() {
        return Err(value_error("maketrans arguments must have same length"));
    }
    let mut table: Vec<u8> = (0u8..=255).collect();
    for (f, t) in from.iter().zip(to.iter()) {
        table[*f as usize] = *t;
    }
    Ok(Object::new_bytes(table))
}

fn bytes_partition(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let sep = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("partition() expected 1 arg"))?,
    )?;
    if sep.is_empty() {
        return Err(value_error("empty separator"));
    }
    let (head, mid, tail) = match memchr::memmem::find(&data, &sep) {
        Some(i) => (
            data[..i].to_vec(),
            sep.clone(),
            data[i + sep.len()..].to_vec(),
        ),
        None => (data, Vec::new(), Vec::new()),
    };
    Ok(Object::new_tuple(vec![
        bytes_like_result(args, head),
        bytes_like_result(args, mid),
        bytes_like_result(args, tail),
    ]))
}

fn bytes_rpartition(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let sep = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("rpartition() expected 1 arg"))?,
    )?;
    if sep.is_empty() {
        return Err(value_error("empty separator"));
    }
    let (head, mid, tail) = match memchr::memmem::rfind(&data, &sep) {
        Some(i) => (
            data[..i].to_vec(),
            sep.clone(),
            data[i + sep.len()..].to_vec(),
        ),
        None => (Vec::new(), Vec::new(), data),
    };
    Ok(Object::new_tuple(vec![
        bytes_like_result(args, head),
        bytes_like_result(args, mid),
        bytes_like_result(args, tail),
    ]))
}

fn bytes_removeprefix(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "removeprefix() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    }
    let data = bytes_data(args)?;
    let prefix = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("removeprefix() expected 1 arg"))?,
    )?;
    let out = if data.starts_with(&prefix) {
        data[prefix.len()..].to_vec()
    } else {
        data
    };
    Ok(bytes_like_result(args, out))
}

fn bytes_removesuffix(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "removesuffix() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    }
    let data = bytes_data(args)?;
    let suffix = bytes_argview(
        args.get(1)
            .ok_or_else(|| type_error("removesuffix() expected 1 arg"))?,
    )?;
    let out = if !suffix.is_empty() && data.ends_with(&suffix) {
        data[..data.len() - suffix.len()].to_vec()
    } else {
        data
    };
    Ok(bytes_like_result(args, out))
}

fn bytes_expandtabs(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if args.len() > 2 {
        return Err(type_error(format!(
            "expandtabs() takes at most 1 argument ({} given)",
            args.len() - 1
        )));
    }
    let data = bytes_data(args)?;
    let mut tabsize_obj = args.get(1).cloned();
    for (k, v) in kwargs {
        if k == "tabsize" {
            tabsize_obj = Some(v.clone());
        } else {
            return Err(type_error(format!(
                "expandtabs() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    let tabsize = match tabsize_obj {
        None => 8,
        Some(o) => o
            .as_i64()
            .ok_or_else(|| type_error("integer argument expected"))?,
    };
    let mut out = Vec::with_capacity(data.len());
    let mut col: i64 = 0;
    for &b in &data {
        match b {
            b'\t' => {
                if tabsize > 0 {
                    let pad = tabsize - (col % tabsize);
                    out.extend(std::iter::repeat_n(b' ', pad as usize));
                    col += pad;
                }
            }
            b'\n' | b'\r' => {
                out.push(b);
                col = 0;
            }
            _ => {
                out.push(b);
                col += 1;
            }
        }
    }
    Ok(bytes_like_result(args, out))
}

/// Shared `center`/`ljust`/`rjust` plumbing: parse `(width,
/// fillchar=b' ')` where fillchar must be a single byte.
fn bytes_pad_args(args: &[Object], name: &str) -> Result<(Vec<u8>, i64, u8), RuntimeError> {
    let data = bytes_data(args)?;
    let width = args
        .get(1)
        .and_then(|o| o.as_i64())
        .ok_or_else(|| type_error(format!("{name}() expected integer width")))?;
    let fill = match args.get(2) {
        None => b' ',
        Some(o) => {
            let v = bytes_argview(o).ok().filter(|v| v.len() == 1);
            match v {
                Some(v) => v[0],
                None => {
                    return Err(type_error(format!(
                        "{name}() argument 2 must be a byte string of length 1, \
                         not '{}'",
                        o.type_name()
                    )))
                }
            }
        }
    };
    Ok((data, width, fill))
}

fn bytes_center(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, width, fill) = bytes_pad_args(args, "center")?;
    let len = data.len() as i64;
    if width <= len {
        return Ok(bytes_like_result(args, data));
    }
    // CPython biases the extra fill to the right except when `width`
    // is odd (`bytes_center` marg computation).
    let marg = width - len;
    let left = marg / 2 + (marg & width & 1);
    let mut out = Vec::with_capacity(width as usize);
    out.extend(std::iter::repeat_n(fill, left as usize));
    out.extend_from_slice(&data);
    out.extend(std::iter::repeat_n(fill, (marg - left) as usize));
    Ok(bytes_like_result(args, out))
}

fn bytes_ljust(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, width, fill) = bytes_pad_args(args, "ljust")?;
    let mut out = data;
    while (out.len() as i64) < width {
        out.push(fill);
    }
    Ok(bytes_like_result(args, out))
}

fn bytes_rjust(args: &[Object]) -> Result<Object, RuntimeError> {
    let (data, width, fill) = bytes_pad_args(args, "rjust")?;
    let len = data.len() as i64;
    let mut out = Vec::with_capacity(width.max(len) as usize);
    out.extend(std::iter::repeat_n(fill, (width - len).max(0) as usize));
    out.extend_from_slice(&data);
    Ok(bytes_like_result(args, out))
}

fn bytes_zfill(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let width = args
        .get(1)
        .and_then(|o| o.as_i64())
        .ok_or_else(|| type_error("zfill() expected integer width"))?;
    let len = data.len() as i64;
    if width <= len {
        return Ok(bytes_like_result(args, data));
    }
    let pad = (width - len) as usize;
    let mut out = Vec::with_capacity(width as usize);
    let body = if !data.is_empty() && (data[0] == b'+' || data[0] == b'-') {
        out.push(data[0]);
        &data[1..]
    } else {
        &data[..]
    };
    out.extend(std::iter::repeat_n(b'0', pad));
    out.extend_from_slice(body);
    Ok(bytes_like_result(args, out))
}

fn bytes_capitalize(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("capitalize", args)?;
    let data = bytes_data(args)?;
    let mut out = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        out.push(if i == 0 {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        });
    }
    Ok(bytes_like_result(args, out))
}

fn bytes_title(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("title", args)?;
    let data = bytes_data(args)?;
    let mut out = Vec::with_capacity(data.len());
    let mut prev_alpha = false;
    for &b in &data {
        if b.is_ascii_alphabetic() {
            out.push(if prev_alpha {
                b.to_ascii_lowercase()
            } else {
                b.to_ascii_uppercase()
            });
            prev_alpha = true;
        } else {
            out.push(b);
            prev_alpha = false;
        }
    }
    Ok(bytes_like_result(args, out))
}

fn bytes_swapcase(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("swapcase", args)?;
    let data = bytes_data(args)?;
    let out: Vec<u8> = data
        .iter()
        .map(|b| {
            if b.is_ascii_uppercase() {
                b.to_ascii_lowercase()
            } else if b.is_ascii_lowercase() {
                b.to_ascii_uppercase()
            } else {
                *b
            }
        })
        .collect();
    Ok(bytes_like_result(args, out))
}

fn bytes_islower(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("islower", args)?;
    let data = bytes_data(args)?;
    let has_cased = data.iter().any(u8::is_ascii_lowercase);
    let no_upper = !data.iter().any(u8::is_ascii_uppercase);
    Ok(Object::Bool(has_cased && no_upper))
}

fn bytes_isupper(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isupper", args)?;
    let data = bytes_data(args)?;
    let has_cased = data.iter().any(u8::is_ascii_uppercase);
    let no_lower = !data.iter().any(u8::is_ascii_lowercase);
    Ok(Object::Bool(has_cased && no_lower))
}

fn bytes_istitle(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("istitle", args)?;
    let data = bytes_data(args)?;
    let mut cased = false;
    let mut prev_cased = false;
    for &b in &data {
        if b.is_ascii_uppercase() {
            if prev_cased {
                return Ok(Object::Bool(false));
            }
            cased = true;
            prev_cased = true;
        } else if b.is_ascii_lowercase() {
            if !prev_cased {
                return Ok(Object::Bool(false));
            }
            cased = true;
            prev_cased = true;
        } else {
            prev_cased = false;
        }
    }
    Ok(Object::Bool(cased))
}

fn bytes_isascii(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isascii", args)?;
    let data = bytes_data(args)?;
    Ok(Object::Bool(data.iter().all(u8::is_ascii)))
}

// ---- bytearray-only mutators beyond append/extend/pop/clear ------

fn bytearray_only(args: &[Object], name: &str) -> Result<Rc<RefCell<Vec<u8>>>, RuntimeError> {
    match args.first() {
        Some(Object::ByteArray(b)) => Ok(b.clone()),
        // Unbound calls on subclass instances (`bytearray.append(me, …)`)
        // reach the native payload.
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::ByteArray(b)) => Ok(b.clone()),
            _ => Err(type_error(format!(
                "{name}() requires a bytearray receiver"
            ))),
        },
        _ => Err(type_error(format!(
            "{name}() requires a bytearray receiver"
        ))),
    }
}

/// `_getbytevalue`: an int in `range(0, 256)` via the full
/// `__index__` protocol (native unwrap or interpreter reentry).
/// Used by `insert`/`remove`/`append` and bytearray item assignment.
pub(crate) fn bytearray_byte_arg(arg: &Object) -> Result<u8, RuntimeError> {
    let native = arg.native_value();
    match native.as_ref().unwrap_or(arg) {
        Object::Bool(v) => Ok(u8::from(*v)),
        Object::Int(v) if (0..=255).contains(v) => Ok(*v as u8),
        Object::Int(_) | Object::Long(_) => Err(value_error("byte must be in range(0, 256)")),
        inst @ Object::Instance(_) if crate::instance_method(inst, "__index__").is_some() => {
            let v = coerce_index_i64(inst)?;
            if (0..=255).contains(&v) {
                Ok(v as u8)
            } else {
                Err(value_error("byte must be in range(0, 256)"))
            }
        }
        other => Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

fn bytearray_insert(args: &[Object]) -> Result<Object, RuntimeError> {
    let cell = bytearray_only(args, "insert")?;
    let pos = args
        .get(1)
        .and_then(|o| o.as_i64())
        .ok_or_else(|| type_error("insert() expected integer index"))?;
    let byte = bytearray_byte_arg(
        args.get(2)
            .ok_or_else(|| type_error("insert() expected 2 args"))?,
    )?;
    crate::object::bytearray_check_resizable(&cell)?;
    let mut data = cell.borrow_mut();
    let len = data.len() as i64;
    let idx = if pos < 0 {
        (len + pos).max(0)
    } else {
        pos.min(len)
    } as usize;
    data.insert(idx, byte);
    Ok(Object::None)
}

fn bytearray_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let cell = bytearray_only(args, "remove")?;
    let byte = bytearray_byte_arg(
        args.get(1)
            .ok_or_else(|| type_error("remove() expected 1 arg"))?,
    )?;
    let mut data = cell.borrow_mut();
    match data.iter().position(|b| *b == byte) {
        Some(i) => {
            crate::object::bytearray_check_resizable(&cell)?;
            data.remove(i);
            Ok(Object::None)
        }
        None => Err(value_error("value not found in bytearray")),
    }
}

fn bytearray_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let cell = bytearray_only(args, "copy")?;
    let data = cell.borrow().clone();
    Ok(Object::new_bytearray(data))
}

fn bytes_isalnum(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isalnum", args)?;
    let data = bytes_data(args)?;
    Ok(Object::Bool(
        !data.is_empty() && data.iter().all(u8::is_ascii_alphanumeric),
    ))
}

fn bytes_isalpha(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isalpha", args)?;
    let data = bytes_data(args)?;
    Ok(Object::Bool(
        !data.is_empty() && data.iter().all(u8::is_ascii_alphabetic),
    ))
}

fn bytes_isdigit(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isdigit", args)?;
    let data = bytes_data(args)?;
    Ok(Object::Bool(
        !data.is_empty() && data.iter().all(u8::is_ascii_digit),
    ))
}

fn bytes_isspace(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_no_args("isspace", args)?;
    let data = bytes_data(args)?;
    // CPython's Py_ISSPACE: space, \t, \n, \v, \f, \r — Rust's
    // `is_ascii_whitespace` omits \x0b (vertical tab).
    Ok(Object::Bool(
        !data.is_empty() && data.iter().copied().all(byte_is_pyspace),
    ))
}

// ---------- bytearray-only mutators ----------

fn bytearray_self(args: &[Object]) -> Result<Rc<crate::sync::RefCell<Vec<u8>>>, RuntimeError> {
    match args.first() {
        Some(Object::ByteArray(b)) => Ok(b.clone()),
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::ByteArray(b)) => Ok(b.clone()),
            _ => Err(type_error("expected bytearray receiver")),
        },
        _ => Err(type_error("expected bytearray receiver")),
    }
}

/// `bytearray.__init__(self, source=None, encoding=None, errors=None)` —
/// (re)initialise the buffer *in place*. CPython's `bytearray___init___`:
/// the content is rebuilt from the constructor arguments; a re-init that
/// changes the length while the buffer is exported is forbidden.
pub(crate) fn bytearray_init_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let new = bytes_construct(&args[1..], kwargs, "bytearray")?;
    if new.len() != b.borrow().len() {
        crate::object::bytearray_check_resizable(&b)?;
    }
    *b.borrow_mut() = new;
    Ok(Object::None)
}

pub(crate) fn bytearray_init(args: &[Object]) -> Result<Object, RuntimeError> {
    bytearray_init_kw(args, &[])
}

fn bytearray_append(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let value = args
        .get(1)
        .ok_or_else(|| type_error("append() takes exactly one argument (0 given)"))?;
    let byte = bytearray_byte_arg(value)?;
    crate::object::bytearray_check_resizable(&b)?;
    b.borrow_mut().push(byte);
    Ok(Object::None)
}

fn bytearray_extend(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("extend() takes exactly 1 argument (0 given)"))?;
    // CPython rejects `str` up front with a dedicated message —
    // strings are iterable but never an "iterable of ints".
    if matches!(
        other.native_value().as_ref().unwrap_or(other),
        Object::Str(_)
    ) {
        return Err(type_error("expected iterable of integers; got: 'str'"));
    }
    // Bytes-like fast paths (with `b.extend(b)` alias safety).
    match other {
        Object::Bytes(buf) => {
            if !buf.is_empty() {
                crate::object::bytearray_check_resizable(&b)?;
            }
            b.borrow_mut().extend_from_slice(buf);
            return Ok(Object::None);
        }
        Object::ByteArray(buf) => {
            if !buf.borrow().is_empty() {
                crate::object::bytearray_check_resizable(&b)?;
            }
            if Rc::ptr_eq(&b, buf) {
                let mut t = b.borrow_mut();
                let copy = t.clone();
                t.extend_from_slice(&copy);
            } else {
                b.borrow_mut().extend_from_slice(&buf.borrow());
            }
            return Ok(Object::None);
        }
        _ => {}
    }
    // Buffer protocol (PEP 3118 / PEP 688): a `memoryview` or any object that
    // exports a buffer (`array.array`, mmap, …) extends with its *raw bytes*,
    // not its iterated items — matching CPython's `bytearray.extend`, which
    // routes any `PyObject_CheckBuffer` argument through `bytearray_setslice`
    // before the integer-iteration fallback. Without this, e.g.
    // `bytearray().extend(array('I', [1000]))` — exactly what
    // `_pyio.BufferedWriter.write` does for `gzip`/`bz2` array writes — would
    // iterate the out-of-range int items and raise.
    if matches!(other, Object::MemoryView(_)) {
        let bytes = other.as_bytes_view().unwrap_or_default();
        if !bytes.is_empty() {
            crate::object::bytearray_check_resizable(&b)?;
        }
        b.borrow_mut().extend_from_slice(&bytes);
        return Ok(Object::None);
    }
    if let Some(method) = crate::instance_method(other, "__buffer__") {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by an enclosing VM frame still live on this
            // thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let view =
                interp.call_object_with_globals(&method, &[Object::Int(0)], &[], &globals)?;
            if let Some(bytes) = view.as_bytes_view() {
                if !bytes.is_empty() {
                    crate::object::bytearray_check_resizable(&b)?;
                }
                b.borrow_mut().extend_from_slice(&bytes);
                return Ok(Object::None);
            }
        }
    }
    // General protocol: any iterable of ints (each through `__index__`,
    // as CPython's `bytearray_extend` does via `_getbytevalue`).
    // Generators and user-`__iter__` objects were materialised by the
    // interpreter's dispatch shim before reaching this builtin.
    let mut it = other
        .make_iter()
        .map_err(|_| type_error(format!("can't extend bytearray with {}", other.type_name())))?;
    // Collect first so a mid-iteration error leaves the target
    // unchanged (CPython builds into a fresh buffer too).
    let mut tmp: Vec<u8> = Vec::new();
    while let Some(item) = it.next_value() {
        tmp.push(bytearray_byte_arg(&item)?);
    }
    if !tmp.is_empty() {
        crate::object::bytearray_check_resizable(&b)?;
    }
    b.borrow_mut().extend_from_slice(&tmp);
    Ok(Object::None)
}

fn bytearray_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    // Resize-to-zero is a no-op on an empty buffer (CPython's
    // `PyByteArray_Resize` short-circuits before `_canresize`).
    if !b.borrow().is_empty() {
        crate::object::bytearray_check_resizable(&b)?;
    }
    b.borrow_mut().clear();
    Ok(Object::None)
}

fn bytearray_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let mut buf = b.borrow_mut();
    if buf.is_empty() {
        return Err(crate::error::index_error("pop from empty bytearray"));
    }
    let idx_arg = args.get(1).cloned().unwrap_or(Object::Int(-1));
    let idx = match idx_arg {
        Object::Int(i) => {
            let len = buf.len() as i64;
            let n = if i < 0 { i + len } else { i };
            if n < 0 || n >= len {
                return Err(crate::error::index_error("pop index out of range"));
            }
            n as usize
        }
        _ => return Err(type_error("pop() index must be int")),
    };
    crate::object::bytearray_check_resizable(&b)?;
    let v = buf.remove(idx);
    Ok(Object::Int(i64::from(v)))
}

fn bytearray_reverse(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    b.borrow_mut().reverse();
    Ok(Object::None)
}

// ---------- file methods ----------

pub(crate) fn file_self(args: &[Object]) -> Result<Rc<crate::object::PyFile>, RuntimeError> {
    match args.first() {
        Some(Object::File(f)) => Ok(f.clone()),
        // A subclass of `io.BytesIO`/`io.StringIO` is a `PyInstance` that
        // wraps the native stream in `native`; unwrap so every inherited
        // file method (read/write/seek/…) operates on the real buffer.
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::File(f)) => Ok(f.clone()),
            _ => Err(type_error("expected file receiver")),
        },
        _ => Err(type_error("expected file receiver")),
    }
}

/// CPython's `CHECK_CLOSED`: an I/O method on a closed stream raises
/// `ValueError("I/O operation on closed file.")` (`test_io.test_io_after_close`).
pub(crate) fn file_check_open(f: &Rc<crate::object::PyFile>) -> Result<(), RuntimeError> {
    if *f.closed.borrow() {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(())
}

/// Convert decoded stream text into a `str` Object, un-bridging the PUA
/// surrogate window when the stream can actually contain bridged lone
/// surrogates (a `StringIO` that received surrogate writes, or a
/// surrogate-producing decode error handler — see
/// `PyFile::unbridge_on_read`). Everything else takes the plain
/// `Object::Str` path so genuine plane-16 PUA characters survive.
fn stream_text_object(f: &Rc<crate::object::PyFile>, s: String) -> Object {
    let bridged = f.unbridge_on_read();
    if bridged {
        bridge_to_object(&s)
    } else {
        Object::from_str(s)
    }
}

pub(crate) fn file_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    // A stream opened write-only (`'w'`, `'a'`, `'x'`, `'wb'`, …) raises
    // `io.UnsupportedOperation` on `read`, not a raw `OSError(EBADF)` from the
    // kernel (`test_io.test_invalid_operations`). CPython gates this in the
    // buffered/raw layer before touching the fd.
    if !f.readable() {
        return Err(crate::stdlib::io::unsupported_op("read"));
    }
    let n = match args.get(1) {
        None | Some(Object::None) => None,
        Some(o) => {
            // The size is parsed with `__index__` (clinic `Py_ssize_t`), so
            // an IntLike works (test_memoryio.test_read). `-1` reads all;
            // other negatives read all too on the in-memory / raw layers,
            // but are a ValueError on a buffered reader (CPython
            // `bufferedreader.c` vs `bytesio.c`).
            let i = match o {
                Object::Int(i) => *i,
                _ => coerce_index_i64(o).map_err(|_| type_error("read() argument must be int"))?,
            };
            if i >= 0 {
                Some(i as usize)
            } else if i == -1
                || matches!(
                    f.io_kind.get(),
                    crate::object::IoKind::Raw
                        | crate::object::IoKind::BytesIO
                        | crate::object::IoKind::StringIO
                )
            {
                None
            } else {
                return Err(value_error("read length must be non-negative or -1"));
            }
        }
    };
    if f.binary {
        // `read_bytes_opt` yields `None` for a would-block on a non-blocking
        // descriptor, mirroring CPython's `BufferedReader.read()` (which can
        // return `None`); `iter(f.read, None)` relies on that sentinel
        // (`test_io.test_nonblock_pipe_write_*`).
        let buffered = !matches!(
            f.io_kind.get(),
            crate::object::IoKind::Raw | crate::object::IoKind::BytesIO
        );
        match f.read_bytes_opt(n)? {
            Some(mut data) => {
                // A *buffered* size-`n` read keeps issuing raw reads until it
                // has `n` bytes or hits EOF/would-block — a raw read cut
                // short (a pipe delivering data in dribs, a signal handler
                // interleaving) is not the end of the stream (CPython
                // `BufferedReader.read`; test_io
                // `check_interrupted_read_retry`). A raw (`FileIO`) read
                // stays single-syscall and may return partial.
                if buffered {
                    if let Some(want) = n {
                        while !data.is_empty() && data.len() < want {
                            match f.read_bytes_opt(Some(want - data.len()))? {
                                Some(more) if more.is_empty() => break,
                                Some(more) => data.extend_from_slice(&more),
                                None => break,
                            }
                        }
                    }
                }
                Ok(Object::new_bytes(data))
            }
            None => Ok(Object::None),
        }
    } else if f.text_incr_active_gate() {
        // A custom incremental-only codec (its one-shot `decode` is `None`,
        // e.g. test_io's `test_decoder`): drive the faithful CPython
        // `TextIOWrapper` incremental machinery. `n` is `None` for a full
        // read, `Some(size)` for a character-counted read.
        Ok(stream_text_object(&f, f.read_text_incr(n)?))
    } else if let Some(count) = n {
        // Text `read(size)` counts *characters*, not bytes (CPython
        // `TextIOWrapper`/`StringIO`); read code-point-wise so a multibyte
        // char is never split at the size boundary.
        Ok(stream_text_object(&f, f.read_text_n(count)?))
    } else {
        match f.read_bytes_opt(None)? {
            Some(data) => Ok(stream_text_object(&f, f.decode_text(data)?)),
            None => Ok(Object::None),
        }
    }
}

/// `BufferedReader.peek([size])` — return buffered bytes without advancing the
/// stream position. CPython returns "an arbitrary amount of data, at least one
/// byte unless EOF, possibly more than requested"; for WeavePy's OS-buffered
/// file-backed reader we materialise up to a buffer's worth, then restore the
/// position. Only reachable on binary buffered readers (`peek` is in the file
/// method table only for `f.binary` readers).
pub(crate) fn file_peek(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    if !f.readable() {
        return Err(crate::stdlib::io::unsupported_op("peek"));
    }
    let pos = f.tell()?;
    let chunk = f.read_bytes(Some(crate::object::DEFAULT_BUFFER_SIZE))?;
    f.seek(pos as isize, 0)?;
    Ok(Object::new_bytes(chunk))
}

/// The native `bytearray` backing an instance (a `bytearray` subclass), if
/// any — used to prefer the direct-mutation path over `__buffer__`.
fn inst_native_bytearray(obj: &Object) -> Option<crate::sync::Rc<RefCell<Vec<u8>>>> {
    match obj {
        Object::ByteArray(b) => Some(b.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::ByteArray(b)) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Acquire a writable `memoryview` over a PEP 688 buffer exporter by calling
/// its `__buffer__` (the writable request flag). Returns `Ok(None)` when the
/// object has no `__buffer__` so the caller can fall through to its
/// "must be read-write bytes-like" error.
fn acquire_writable_view(
    obj: &Object,
) -> Result<Option<crate::sync::Rc<crate::object::PyMemoryView>>, RuntimeError> {
    let Some(method) = crate::instance_method(obj, "__buffer__") else {
        return Ok(None);
    };
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(None);
    };
    // SAFETY: published by an enclosing VM frame still live on this thread;
    // the GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    // `inspect.BufferFlags.WRITABLE` is `0x200`; an exporter that can't
    // satisfy a writable request raises, which propagates as CPython's does.
    let r = interp.call_object_with_globals(&method, &[Object::Int(0x200)], &[], &globals)?;
    match r {
        Object::MemoryView(mv) => Ok(Some(mv)),
        _ => Ok(None),
    }
}

/// `f.readinto(b)` — read up to `len(b)` bytes into a writable
/// bytes-like object, returning the count actually read. Only reachable
/// on binary-mode files (the method table gates on `f.binary`).
pub(crate) fn file_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // Arity before state: `f.readinto()` with no buffer is a TypeError even
    // on a closed file (CPython's argument clinic parses first; test_fileio
    // `testMethods`).
    if args.len() < 2 {
        return Err(type_error("readinto() takes exactly 1 argument (0 given)"));
    }
    file_check_open(&f)?;
    if !f.readable() {
        return Err(crate::stdlib::io::unsupported_op("read"));
    }
    // A writable `memoryview` is a valid target (CPython's `readinto`
    // accepts any read-write buffer). asyncio's `_sock_sendfile_fallback`
    // hands `file.readinto` a `memoryview(bytearray(...))`.
    if let Some(Object::MemoryView(mv)) = args.get(1) {
        if mv.released.get() {
            return Err(value_error(
                "operation forbidden on released memoryview object",
            ));
        }
        if mv.readonly.get() || !mv.is_c_contiguous() {
            return Err(type_error(
                "readinto() argument must be read-write bytes-like object, not memoryview",
            ));
        }
        let capacity = mv.len.get();
        let bytes = f.read_bytes(Some(capacity))?;
        let n = bytes.len();
        let start = mv.start.get();
        let wrote = mv
            .buffer
            .with_write(|d| d[start..start + n].copy_from_slice(&bytes));
        if wrote.is_none() {
            return Err(type_error(
                "readinto() argument must be read-write bytes-like object, not memoryview",
            ));
        }
        return Ok(Object::Int(n as i64));
    }
    // A buffer-protocol object (`array.array`, `mmap`, any PEP 688
    // `__buffer__` exporter) whose `__buffer__` yields a writable memoryview is
    // a valid `readinto` target — the write must propagate to its storage.
    if let Some(obj @ Object::Instance(_)) = args.get(1) {
        if inst_native_bytearray(obj).is_none() {
            if let Some(mv) = acquire_writable_view(obj)? {
                if mv.readonly.get() || !mv.is_c_contiguous() {
                    return Err(type_error(format!(
                        "readinto() argument must be read-write bytes-like object, not {}",
                        obj.type_name()
                    )));
                }
                let capacity = mv.len.get();
                let bytes = f.read_bytes(Some(capacity))?;
                let n = bytes.len();
                let start = mv.start.get();
                let wrote = mv
                    .buffer
                    .with_write(|d| d[start..start + n].copy_from_slice(&bytes));
                if wrote.is_none() {
                    return Err(type_error(format!(
                        "readinto() argument must be read-write bytes-like object, not {}",
                        obj.type_name()
                    )));
                }
                return Ok(Object::Int(n as i64));
            }
        }
    }
    let target = match args.get(1) {
        Some(Object::ByteArray(b)) => b.clone(),
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::ByteArray(b)) => b.clone(),
            _ => {
                return Err(type_error(format!(
                    "readinto() argument must be read-write bytes-like object, not {}",
                    inst.cls().name
                )))
            }
        },
        other => {
            return Err(type_error(format!(
                "readinto() argument must be read-write bytes-like object, not {}",
                other.map_or("nothing", |o| o.type_name())
            )))
        }
    };
    let capacity = target.borrow().len();
    let bytes = f.read_bytes(Some(capacity))?;
    let n = bytes.len();
    target.borrow_mut()[..n].copy_from_slice(&bytes);
    Ok(Object::Int(n as i64))
}

pub(crate) fn file_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    if !f.readable() {
        return Err(crate::stdlib::io::unsupported_op("read"));
    }
    // `readline(size)` caps the bytes read (CPython `IOBase.readline`): a
    // non-negative `size` stops after that many bytes even without a newline.
    // A negative size (or `None`) means "no limit"; a non-integer (e.g. the
    // `5.3` in `test_io.test_readline`) is a `TypeError`, matching CPython's
    // `__index__`-based argument coercion.
    let limit = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Bool(b)) => Some(usize::from(*b)),
        Some(Object::Int(n)) if *n >= 0 => Some(*n as usize),
        Some(Object::Int(_)) => None,
        // `__index__`-based coercion (an IntLike works,
        // test_memoryio.test_readline); anything else keeps CPython's
        // "cannot be interpreted as an integer" TypeError.
        Some(other) => match coerce_index_i64(other) {
            Ok(n) if n >= 0 => Some(n as usize),
            Ok(_) => None,
            Err(_) => {
                return Err(type_error(format!(
                    "'{}' object cannot be interpreted as an integer",
                    other.type_name()
                )))
            }
        },
    };
    // A byte-backed text stream on a newline-unsafe codec (UTF-16/32) or a
    // custom incremental-only codec must find the line boundary in *decoded*
    // text — raw byte scanning would split multi-byte code units and corrupt
    // the decode. Route these through the faithful `TextIOWrapper` readline.
    if !f.binary && f.text_incr_active_gate() {
        return Ok(stream_text_object(&f, f.readline_text_incr(limit)?));
    }
    // Where a line ends depends on the stream's newline policy; the shared
    // `PyFile::read_line_bytes` scans raw bytes for the right terminator
    // (binary/`\n`/`\r`/`\r\n`/universal) and `decode_text` then applies any
    // newline *translation*. The VM's native file iteration
    // (`PyIterator::File` → `readline_obj`) uses the same core, so explicit
    // `readline()` and `for line in f` split identically.
    let out = f.read_line_bytes(limit)?;
    if f.binary {
        Ok(Object::new_bytes(out))
    } else {
        Ok(stream_text_object(&f, f.decode_text(out)?))
    }
}

/// `next(file)` — return the next line, or raise StopIteration at EOF.
/// Backs both the `__next__` method and the VM's native file iteration.
pub(crate) fn file_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // CPython's `TextIOWrapper.__next__` disables `tell()` for the duration of
    // the iteration (the readahead snapshot makes the position ambiguous); a
    // binary stream keeps `tell()` live (`test_io.test_telling`).
    let is_text = !f.binary;
    if is_text {
        f.telling.set(false);
    }
    let line = file_readline(args)?;
    let empty = match &line {
        Object::Str(s) => s.is_empty(),
        // A `WStr` always holds >= 1 lone surrogate, so it is never empty.
        Object::WStr(_) => false,
        Object::Bytes(b) => b.is_empty(),
        _ => true,
    };
    if empty {
        // Iterator exhausted: CPython restores `telling` to `seekable` and
        // raises `StopIteration`.
        if is_text {
            f.telling.set(f.seekable());
        }
        Err(stop_iteration())
    } else {
        Ok(line)
    }
}

pub(crate) fn file_readlines(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    // `readlines(hint)` stops once the accumulated length reaches `hint`
    // (CPython `IOBase.readlines`); zero/negative/None means "no limit".
    // The argument goes through `__index__` (test_memoryio.test_readlines).
    let hint = match args.get(1) {
        None | Some(Object::None) => None,
        Some(o) => match coerce_index_i64(o) {
            Ok(n) if n > 0 => Some(n as usize),
            Ok(_) => None,
            Err(e) => return Err(e),
        },
    };
    let mut lines: Vec<Object> = Vec::new();
    let mut total = 0usize;
    loop {
        let line = file_readline(&[Object::File(f.clone())])?;
        let len = match &line {
            Object::Str(s) => str_char_len(s),
            Object::WStr(cps) => cps.len(),
            Object::Bytes(b) => b.len(),
            _ => 0,
        };
        if len == 0 && !matches!(&line, Object::WStr(_)) {
            break;
        }
        lines.push(line);
        total += len;
        if let Some(h) = hint {
            if total >= h {
                break;
            }
        }
    }
    Ok(Object::new_list(lines))
}

pub(crate) fn file_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // Arity before state: `f.write()` with no data is a TypeError even on a
    // closed file (CPython's clinic parses first; test_fileio `testMethods`).
    let data = args
        .get(1)
        .ok_or_else(|| type_error("write() takes exactly 1 argument (0 given)"))?;
    file_check_open(&f)?;
    // A read-only stream raises `io.UnsupportedOperation` on `write`
    // (`test_io.test_invalid_operations`), before any type-checking of the
    // argument.
    if !f.writable() {
        return Err(crate::stdlib::io::unsupported_op("write"));
    }
    let n = match data {
        Object::Str(s) => {
            // A binary stream (`io.BytesIO`, `open(..., 'wb')`) rejects text,
            // exactly like CPython's `BufferedWriter`/`BytesIO`.
            if f.binary {
                return Err(type_error("a bytes-like object is required, not 'str'"));
            }
            // Text writes commit fully and report the *character* count
            // (CPython `TextIOWrapper.write`), never a partial byte tally.
            f.write_text_all(&f.encode_text(s)?)?;
            s.chars().count()
        }
        // A surrogate-bearing `str`. For an in-memory `StringIO` the lone
        // surrogates ride through the PUA bridge so they round-trip; a real
        // encoded text stream encodes the *actual* code points through its
        // codec + error handler (so strict UTF-8 raises `UnicodeEncodeError`
        // on a lone surrogate, `surrogateescape`/`surrogatepass` round-trip).
        Object::WStr(cps) => {
            if f.binary {
                return Err(type_error("a bytes-like object is required, not 'str'"));
            }
            if matches!(
                &*f.backend.borrow(),
                crate::object::FileBackend::MemText { .. }
            ) {
                let bridged = bridge_encode_cps(cps);
                f.write_text_all(&f.encode_text(&bridged)?)?;
                // Read paths must now un-bridge (see `PyFile::mem_bridged`).
                f.mem_bridged.set(true);
            } else {
                f.write_text_all(&f.encode_text_codepoints(cps)?)?;
            }
            cps.len()
        }
        Object::Bytes(b) => {
            if !f.binary {
                return Err(type_error("string argument expected, got 'bytes'"));
            }
            f.write_bytes(b)?
        }
        Object::ByteArray(b) => {
            if !f.binary {
                return Err(type_error("string argument expected, got 'bytearray'"));
            }
            f.write_bytes(&b.borrow())?
        }
        // Any buffer-protocol object is accepted by a binary stream — CPython's
        // `BufferedWriter.write`/`BytesIO.write` take `memoryview`/`array`/… via
        // the buffer interface. `pathlib.Path.write_bytes` relies on this
        // (it wraps the data in a `memoryview` before writing).
        Object::MemoryView(mv) => {
            if !f.binary {
                return Err(type_error("string argument expected, got 'memoryview'"));
            }
            f.write_bytes(&mv.to_bytes())?
        }
        // A `str`/`bytes` subclass instance writes its wrapped native
        // payload (CPython's argument checks are `isinstance`-based).
        Object::Instance(inst)
            if matches!(
                inst.native.get(),
                Some(Object::Str(_) | Object::WStr(_) | Object::Bytes(_))
            ) =>
        {
            let payload = inst.native.get().cloned().expect("checked above");
            return file_write(&[Object::File(f.clone()), payload]);
        }
        other => {
            // A text stream only accepts `str`; a binary stream accepts any
            // buffer-protocol object (`array.array`, `mmap`, a PEP 688
            // `__buffer__` exporter), matching CPython's `FileIO`/`BytesIO`.
            if !f.binary {
                return Err(type_error(format!(
                    "write() argument must be str, not {}",
                    other.type_name()
                )));
            }
            f.write_bytes(&bytes_argview(other)?)?
        }
    };
    Ok(Object::Int(n as i64))
}

pub(crate) fn file_writelines(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // Arity before state (test_fileio `testMethods`).
    let it = args
        .get(1)
        .ok_or_else(|| type_error("writelines() takes exactly 1 argument (0 given)"))?;
    file_check_open(&f)?;
    if !f.writable() {
        return Err(crate::stdlib::io::unsupported_op("write"));
    }
    let mut iter = it.make_iter()?;
    while let Some(v) = iter.next_value() {
        match v {
            // A *text* stream encodes str lines; a binary stream must reject
            // them like `write()` does — `writelines("abc")` iterates the
            // string and each 1-char str line is a TypeError (test_fileio
            // `testWritelinesError`).
            Object::Str(s) if !f.binary => {
                f.write_bytes(&f.encode_text(&s)?)?;
            }
            Object::Bytes(b) => {
                f.write_bytes(&b)?;
            }
            // A binary stream accepts any buffer-protocol item (`array.array`,
            // `memoryview`, …), mirroring CPython's `writelines`.
            other if f.binary => {
                f.write_bytes(&bytes_argview(&other)?)?;
            }
            other => {
                return Err(type_error(format!(
                    "writelines() argument must be a list of strings, not '{}'",
                    other.type_name()
                )))
            }
        }
    }
    Ok(Object::None)
}

pub(crate) fn file_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // CPython's `IOBase.flush` is a no-op error on a closed stream
    // (`test_io.test_io_after_close`): flush after close raises `ValueError`.
    file_check_open(&f)?;
    f.flush()?;
    Ok(Object::None)
}

/// `TextIOWrapper.reconfigure(*, encoding, errors, newline, line_buffering,
/// write_through)` on the collapsed native text stream. CPython semantics
/// (`_pyio.TextIOWrapper.reconfigure`): an *absent* parameter keeps the
/// current setting; `encoding=None` / `errors=None` also mean "unchanged"
/// (except `errors` resets to `'strict'` when a new encoding arrives
/// without one); an *explicit* `newline=None` selects universal-newline
/// mode. The stdio streams are native `Object::File`s, and CPython's
/// regrtest reconfigures them at startup
/// (`sys.stdout.reconfigure(errors="backslashreplace")`).
pub(crate) fn file_reconfigure(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    if args.len() > 1 {
        return Err(type_error("reconfigure() takes 0 positional arguments"));
    }
    let as_opt_str = |k: &str, v: &Object| -> Result<Option<String>, RuntimeError> {
        match v {
            Object::None => Ok(None),
            Object::Str(_) | Object::WStr(_) => Ok(Some(v.to_str())),
            other => Err(type_error(format!(
                "reconfigure() argument '{k}' must be str or None, not {}",
                other.type_name_owned()
            ))),
        }
    };
    let mut encoding: Option<String> = None;
    let mut errors: Option<String> = None;
    let mut newline: Option<Option<String>> = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "encoding" => encoding = as_opt_str(k, v)?,
            "errors" => errors = as_opt_str(k, v)?,
            "newline" => {
                let nl = as_opt_str(k, v)?;
                if let Some(nl) = &nl {
                    if !matches!(nl.as_str(), "" | "\n" | "\r" | "\r\n") {
                        return Err(value_error(format!("illegal newline value: {nl:?}")));
                    }
                }
                newline = Some(nl);
            }
            "line_buffering" | "write_through" => {
                if !matches!(v, Object::None) {
                    f.set_extra_attr(k, Object::Bool(v.is_truthy()));
                }
            }
            other => {
                return Err(type_error(format!(
                    "reconfigure() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    // CPython flushes pending output before switching the codec state.
    f.flush()?;
    if let Some(enc) = encoding {
        // The 'locale' pseudo-encoding resolves to the current locale's
        // codeset, like `open(..., encoding='locale')`.
        let resolved = if enc.eq_ignore_ascii_case("locale") {
            crate::stdlib::locale_mod::current_codeset()
        } else {
            enc
        };
        f.set_encoding(&resolved);
        // A new encoding without an explicit handler resets to 'strict'.
        if errors.is_none() {
            errors = Some("strict".to_owned());
        }
    }
    if let Some(err) = errors {
        *f.errors.borrow_mut() = Some(err);
    }
    if let Some(nl) = newline {
        *f.newline.borrow_mut() = nl;
    }
    Ok(Object::None)
}

pub(crate) fn file_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // A live `getbuffer()` export pins the BytesIO buffer: closing would
    // free it out from under the view, so CPython raises BufferError
    // (test_memoryio.test_getbuffer closes with an exported view).
    if !*f.closed.borrow() {
        if let crate::object::FileBackend::MemBytes { data, .. } = &*f.backend.borrow() {
            crate::object::bytearray_check_resizable(data)?;
        }
    }
    // CPython's `IOBase.close` calls `self.flush()` *virtually*, so a
    // monkeypatched instance-level `flush` runs at close time. `test_io`'s
    // `test_flush_error_on_close` patches `f.flush` to raise `OSError` and
    // asserts `close()` re-raises it *and* still leaves the file closed (the
    // descriptor is released even when the flush fails). Honour an override by
    // running it, then closing without the native flush.
    if !*f.closed.borrow() {
        // A subclass instance dispatches `flush` *virtually* too — CPython's
        // `IOBase.close` calls `self.flush()` through the type, so a subclass
        // `flush` override runs at close time (`test_io.test_destructor`
        // records close→flush).
        let flush_override = f.get_extra_attr("flush").or_else(|| {
            if let Some(inst @ Object::Instance(_)) = args.first() {
                let ptr = crate::vm_singletons::current_interpreter_ptr()?;
                // SAFETY: published by the enclosing VM frame on this thread.
                let interp = unsafe { &mut *ptr };
                match interp.load_attr_public(inst, "flush") {
                    // The inherited native `flush` (a bound builtin) is what
                    // the non-override path below already does; only a
                    // Python-level override needs the virtual call.
                    Ok(Object::Builtin(_)) => None,
                    Ok(Object::BoundMethod(bm)) => {
                        if matches!(bm.function, Object::Builtin(_)) {
                            None
                        } else {
                            Some(Object::BoundMethod(bm))
                        }
                    }
                    Ok(m) => Some(m),
                    Err(_) => None,
                }
            } else {
                None
            }
        });
        if let Some(flush_fn) = flush_override {
            let flush_res = (|| -> Result<Object, RuntimeError> {
                let ptr = crate::vm_singletons::current_interpreter_ptr()
                    .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
                // SAFETY: published by the enclosing VM frame on this thread.
                let interp = unsafe { &mut *ptr };
                interp.call_object(flush_fn, &[], &[])
            })();
            let close_res = f.close_with_flush();
            flush_res?;
            close_res?;
            return Ok(Object::None);
        }
    }
    // `close()` flushes the staged write buffer first; a flush failure (broken
    // pipe) propagates while the descriptor is still released (CPython
    // `BufferedWriter.close`).
    f.close_with_flush()?;
    Ok(Object::None)
}

pub(crate) fn file_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // Arity before state: `f.seek()` with no position is a TypeError even on
    // a closed file (test_fileio `testMethods`).
    if args.len() < 2 {
        return Err(type_error("seek() takes at least 1 argument (0 given)"));
    }
    file_check_open(&f)?;
    // An explicit seek re-enables `tell()` after an iteration disabled it
    // (CPython restores `telling = seekable` in `textiowrapper_seek`).
    f.telling.set(true);
    let whence = match args.get(2) {
        Some(Object::Int(i)) => *i as i32,
        None => 0,
        _ => return Err(type_error("seek() whence must be int")),
    };
    // Incremental text cookie path (a custom decode=None codec): the seek
    // argument is an opaque `TextIOWrapper` cookie — a Python int that can
    // exceed 64 bits (`Object::Long`) — not a byte offset, so it must be
    // handled before the byte-offset parse below.
    if f.text_incr_active_gate() && f.readable() {
        let cookie = args.get(1).cloned().unwrap_or(Object::Int(0));
        return f.seek_text(&cookie, whence);
    }
    let offset = match args.get(1) {
        Some(Object::Int(i)) => *i as isize,
        Some(Object::Bool(b)) => isize::from(*b),
        // A Python int beyond i64 is a distinct `Object::Long`; CPython's
        // argument clinic raises OverflowError converting it to
        // Py_ssize_t, and callers rely on catching exactly that
        // (plistlib treats it as a corrupt binary plist).
        Some(Object::Long(_)) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C ssize_t",
            ));
        }
        _ => return Err(type_error("seek() expected int")),
    };
    // A text stream (CPython's `TextIOWrapper`) only supports absolute seeks to
    // opaque cookies; a non-zero current- or end-relative seek raises
    // `io.UnsupportedOperation` (`test_io.test_invalid_operations`).
    if !f.binary && offset != 0 && (whence == 1 || whence == 2) {
        let msg = if whence == 1 {
            "can't do nonzero cur-relative seeks"
        } else {
            "can't do nonzero end-relative seeks"
        };
        return Err(crate::stdlib::io::unsupported_op(msg));
    }
    Ok(Object::Int(f.seek(offset, whence)? as i64))
}

pub(crate) fn file_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    // A text stream mid-iteration has `tell()` disabled (CPython
    // `textiowrapper_tell`: `telling` cleared by `__next__`).
    if !f.binary && !f.telling.get() {
        return Err(crate::error::os_error(
            "telling position disabled by next() call",
        ));
    }
    // Incremental text cookie path (a custom decode=None codec): `tell()`
    // returns an opaque decoder-state cookie, not a byte offset.
    if f.text_incr_active_gate() && f.readable() {
        return f.tell_text_incr();
    }
    Ok(Object::Int(f.tell()? as i64))
}

pub(crate) fn file_truncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    let size = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Bool(b)) => Some(u64::from(*b)),
        // `__index__` conversion, then the sign check — an IntLike(-1) is
        // the same ValueError as a plain -1 (test_memoryio.test_truncate).
        Some(o) => {
            let i = match o {
                Object::Int(i) => *i,
                _ => coerce_index_i64(o)?,
            };
            if i < 0 {
                return Err(value_error(format!("negative size value {i}")));
            }
            Some(i as u64)
        }
    };
    Ok(Object::Int(f.truncate(size)? as i64))
}

pub(crate) fn file_isatty(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // `isatty()` on a closed stream raises (`test_io.test_io_after_close`).
    if *f.closed.borrow() {
        return Err(value_error("I/O operation on closed file"));
    }
    Ok(Object::Bool(f.isatty()))
}

pub(crate) fn file_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    if *f.closed.borrow() {
        return Err(value_error("I/O operation on closed file"));
    }
    match f.fileno() {
        Some(fd) => Ok(Object::Int(fd)),
        // In-memory `BytesIO`/`StringIO` have no descriptor: CPython raises
        // `io.UnsupportedOperation` (a subclass of OSError and ValueError).
        None => Err(crate::stdlib::io::unsupported_op("fileno")),
    }
}

// The three ability predicates raise `ValueError` once the *object* is closed
// (CPython's `err_closed` when `self->fd < 0`) — but not when the descriptor
// was merely closed out from under a live object (`os.close(f.fileno())`),
// where the cached ability still answers (test_fileio `testMethods` vs
// `testErrnoOnClosedSeekable`).
pub(crate) fn file_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    Ok(Object::Bool(f.readable()))
}

pub(crate) fn file_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    Ok(Object::Bool(f.writable()))
}

pub(crate) fn file_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    file_check_open(&f)?;
    Ok(Object::Bool(f.seekable()))
}

// `IOBase._checkReadable/_checkWritable/_checkSeekable/_checkClosed` — the
// private protocol helpers CPython's `io` objects expose. The layered `_pyio`
// Buffered*/TextIOWrapper classes call these on the raw stream they wrap (e.g.
// `BufferedRandom.__init__` does `raw._checkSeekable()`); native `Object::File`
// raws must answer them, raising `io.UnsupportedOperation`/`ValueError` exactly
// as CPython's `_io._IOBase` does.
pub(crate) fn file_check_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    if !file_self(args)?.readable() {
        return Err(crate::stdlib::io::unsupported_op(
            "File or stream is not readable.",
        ));
    }
    Ok(Object::None)
}

pub(crate) fn file_check_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    if !file_self(args)?.writable() {
        return Err(crate::stdlib::io::unsupported_op(
            "File or stream is not writable.",
        ));
    }
    Ok(Object::None)
}

pub(crate) fn file_check_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    if !file_self(args)?.seekable() {
        return Err(crate::stdlib::io::unsupported_op(
            "File or stream is not seekable.",
        ));
    }
    Ok(Object::None)
}

pub(crate) fn file_check_closed(args: &[Object]) -> Result<Object, RuntimeError> {
    if *file_self(args)?.closed.borrow() {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(Object::None)
}

/// `RawIOBase.readall()` — read and return all bytes until EOF. CPython's
/// `_pyio.BufferedReader` falls back to `raw.readall()` for a full read; a
/// native binary `Object::File` answers it as a sizeless `read()`.
pub(crate) fn file_readall(args: &[Object]) -> Result<Object, RuntimeError> {
    let self_arg = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("readall() requires a file"))?;
    file_read(std::slice::from_ref(&self_arg))
}

/// The PEP-307 instance-`__dict__` slot for an in-memory stream's pickle
/// state. For a *subclass* (`Object::Instance` wrapping the native stream) the
/// attributes live in the instance dict; for the base `Object::File` they live
/// in `extra_attrs`. CPython's base streams expose `None` until an attribute is
/// set, so an empty store reads back as `None`.
fn mem_dict_slot(receiver: &Object) -> Object {
    match receiver {
        Object::Instance(inst) => {
            let d = inst.dict.borrow();
            if d.is_empty() {
                Object::None
            } else {
                Object::Dict(Rc::new(RefCell::new(d.clone())))
            }
        }
        Object::File(f) => {
            let attrs = f.extra_attrs.borrow();
            if attrs.is_empty() {
                Object::None
            } else {
                let mut d = crate::object::DictData::default();
                for (k, v) in attrs.iter() {
                    d.insert(DictKey(Object::from_str(k.clone())), v.clone());
                }
                Object::Dict(Rc::new(RefCell::new(d)))
            }
        }
        _ => Object::None,
    }
}

/// Restore the instance-`__dict__` slot from a pickle state onto the receiver
/// (subclass instance dict, or base stream `extra_attrs`).
fn mem_apply_dict(receiver: &Object, dict: &Rc<RefCell<DictData>>) {
    match receiver {
        Object::Instance(inst) => {
            let mut inst_dict = inst.dict.borrow_mut();
            for (k, v) in dict.borrow().iter() {
                inst_dict.insert(k.clone(), v.clone());
            }
        }
        Object::File(f) => {
            for (k, v) in dict.borrow().iter() {
                if let Object::Str(name) = &k.0 {
                    f.set_extra_attr(name, v.clone());
                }
            }
        }
        _ => {}
    }
}

/// `BytesIO.__getstate__` / `StringIO.__getstate__`. Unlike file-backed
/// streams, CPython's in-memory streams *are* picklable; the state tuple
/// mirrors `Modules/_io/{bytesio,stringio}.c`:
///   * `BytesIO`  → `(buffer: bytes, pos: int, dict | None)`
///   * `StringIO` → `(value: str, newline: str, pos: int, dict | None)`
/// The trailing slot is the instance `__dict__` (or `None` when empty). A
/// closed stream raises `ValueError`, exactly like CPython.
pub(crate) fn file_getstate_mem(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::FileBackend;
    let receiver = args
        .first()
        .ok_or_else(|| type_error("__getstate__ requires a stream"))?;
    let f = file_self(args)?;
    if !f.is_memory() {
        // File-backed streams stay unpicklable (`file_reduce_forbidden`).
        return file_reduce_forbidden(args);
    }
    if *f.closed.borrow() {
        return Err(value_error("__getstate__ on closed file"));
    }
    let pos = f.position() as i64;
    let dict_slot = mem_dict_slot(receiver);
    let value = f
        .getvalue()
        .ok_or_else(|| type_error("not an in-memory stream"))?;
    let is_text = matches!(&*f.backend.borrow(), FileBackend::MemText { .. });
    if is_text {
        // StringIO's newline policy: the field holds `Some(s)` for an
        // explicit `newline=` (default `'\n'`) and `None` for universal
        // mode, which must round-trip as pickled `None` so translation
        // survives unpickling (test_memoryio CStringIOPickleTest).
        let nl = match f.newline.borrow().clone() {
            Some(s) => Object::from_str(s),
            None => Object::None,
        };
        Ok(Object::new_tuple(vec![
            value,
            nl,
            Object::Int(pos),
            dict_slot,
        ]))
    } else {
        Ok(Object::new_tuple(vec![value, Object::Int(pos), dict_slot]))
    }
}

/// `BytesIO.__reduce__` / `__reduce_ex__` / `StringIO.__reduce__` for the *base*
/// `Object::File` stream — reconstruct via `(cls, (), state)`: `cls()` mints an
/// empty stream and `__setstate__` refills it. `cls` pickles by reference now
/// that the concrete types report `__module__ == "_io"`, so
/// `pickle.loads(pickle.dumps(BytesIO(...)))` and `copy.copy`/`copy.deepcopy`
/// round-trip. (Subclass instances go through `object.__reduce_ex__`'s
/// `__newobj__` path instead, since their `__init__` may require arguments.)
/// File-backed streams fall back to the forbidding reducer.
pub(crate) fn file_reduce_mem(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    if !f.is_memory() {
        return file_reduce_forbidden(args);
    }
    let cls = Object::Type(class_of(&Object::File(f.clone())));
    // `__reduce_ex__(self, protocol)` carries the protocol in `args[1]`; the
    // reconstruction tuple is protocol-independent, so it is ignored. Reuse the
    // same `self` slot for `__getstate__`.
    let state = file_getstate_mem(&args[..1])?;
    Ok(Object::new_tuple(vec![
        cls,
        Object::new_tuple(Vec::new()),
        state,
    ]))
}

/// Validate a `StringIO` newline value (pickle state slot 1). CPython accepts
/// `None` / `""` / `"\n"` / `"\r"` / `"\r\n"`, raising `ValueError` for any
/// other string and `TypeError` for a non-string.
fn validate_stringio_newline(value: &Object) -> Result<Option<String>, RuntimeError> {
    match value {
        Object::None => Ok(None),
        Object::Str(s) => match s.as_ref() {
            "" | "\n" | "\r" | "\r\n" => Ok(Some(s.to_string())),
            other => Err(value_error(format!("illegal newline value: '{other}'"))),
        },
        _ => Err(type_error("newline must be str or None")),
    }
}

/// `BytesIO.__setstate__` / `StringIO.__setstate__` — restore buffer, position,
/// newline (StringIO), and instance dict from a `__getstate__` tuple. The
/// validation order and error types mirror `bytesio.c`/`stringio.c` exactly
/// (test_memoryio `test_setstate`): closed → `ValueError`; non-tuple / short
/// tuple / wrong-typed buffer/newline/position / non-dict slot → `TypeError`;
/// negative position or illegal newline → `ValueError`. All inputs are
/// validated *before* any mutation.
pub(crate) fn file_setstate_mem(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::FileBackend;
    let receiver = args
        .first()
        .ok_or_else(|| type_error("__setstate__ requires a stream"))?;
    let f = file_self(args)?;
    let is_text = matches!(&*f.backend.borrow(), FileBackend::MemText { .. });
    let kind = if is_text { "StringIO" } else { "BytesIO" };
    let want = if is_text { 4 } else { 3 };
    // (1) A closed stream rejects setstate with ValueError.
    if *f.closed.borrow() {
        return Err(value_error(format!("__setstate__ on closed {kind}")));
    }
    // (2) The state must be a tuple of the right arity.
    let items: &[Object] = match args.get(1) {
        Some(Object::Tuple(t)) if t.len() >= want => t,
        _ => {
            return Err(type_error(format!(
                "{kind}.__setstate__ argument should be a {want}-tuple, got something else"
            )))
        }
    };
    // (3) The instance-dict slot (last element) must be a dict or None.
    let dict_to_apply = match &items[want - 1] {
        Object::None => None,
        Object::Dict(d) => Some(d.clone()),
        _ => {
            return Err(type_error(format!(
                "{} item of state should be a dict",
                if is_text { "fourth" } else { "third" }
            )))
        }
    };
    if is_text {
        // (4) value: str; (5) newline: valid str/None; (6) pos: non-negative int.
        let txt = match &items[0] {
            Object::Str(s) => s.to_string(),
            // Restore a surrogate-bearing buffer through the PUA bridge.
            Object::WStr(cps) => bridge_encode_cps(cps),
            _ => return Err(type_error("initial_value must be str or None")),
        };
        let newline = validate_stringio_newline(&items[1])?;
        let pos = pos_from_state(&items[2], "third")?;
        if let FileBackend::MemText { data, pos: tpos } = &mut *f.backend.borrow_mut() {
            *data = txt;
            // The pickled position counts characters; the backend stores a
            // byte offset (see `memtext_byte_of_char`).
            *tpos = crate::object::memtext_byte_of_char(data, pos);
        }
        f.set_newline(newline.as_deref());
    } else {
        // (4) buffer: bytes-like; (5) pos: non-negative int.
        let buf = match &items[0] {
            Object::Bytes(b) => b.to_vec(),
            Object::ByteArray(b) => b.borrow().clone(),
            _ => return Err(type_error("a bytes-like object is required")),
        };
        let pos = pos_from_state(&items[1], "second")?;
        if let FileBackend::MemBytes { data, pos: bpos } = &mut *f.backend.borrow_mut() {
            *data.borrow_mut() = buf;
            *bpos = pos;
        }
    }
    if let Some(d) = dict_to_apply {
        mem_apply_dict(receiver, &d);
    }
    Ok(Object::None)
}

/// Decode a pickle-state position slot: must be a non-negative int
/// (`TypeError` otherwise, `ValueError` if negative — matching CPython).
fn pos_from_state(slot: &Object, ordinal: &str) -> Result<usize, RuntimeError> {
    match slot {
        Object::Int(n) if *n >= 0 => Ok(*n as usize),
        Object::Int(_) => Err(value_error("position value cannot be negative")),
        _ => Err(type_error(format!(
            "{ordinal} item of state must be an integer"
        ))),
    }
}

/// `IOBase.__getstate__` / `__reduce_ex__` — CPython forbids pickling stream
/// objects (`TypeError: cannot pickle '_io.X' object`). Native `Object::File`
/// streams mirror that so `test_io`'s `test_pickling` (which asserts the
/// TypeError) passes.
pub(crate) fn file_reduce_forbidden(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let name = if !f.binary {
        "_io.TextIOWrapper"
    } else if matches!(
        &*f.backend.borrow(),
        crate::object::FileBackend::MemBytes { .. }
    ) {
        "_io.BytesIO"
    } else if f.writable() && f.readable() {
        "_io.BufferedRandom"
    } else if f.writable() {
        "_io.BufferedWriter"
    } else {
        "_io.BufferedReader"
    };
    Err(type_error(format!("cannot pickle '{name}' object")))
}

pub(crate) fn file_getbuffer(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // A closed BytesIO has no buffer to export (test_memoryio.test_getbuffer
    // closes then asserts ValueError).
    file_check_open(&f)?;
    f.getbuffer()
}

pub(crate) fn file_getvalue(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    // Closed in-memory streams refuse (`test_memoryio.test_truncate` closes
    // then asserts ValueError).
    file_check_open(&f)?;
    f.getvalue()
        .ok_or_else(|| type_error("getvalue() requires StringIO/BytesIO"))
}

pub(crate) fn file_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `IOBase.__enter__` runs `self._checkClosed()` first, so
    // re-entering a closed file (e.g. `with already_closed_tempfile:`) raises
    // `ValueError` rather than silently succeeding.
    let f = file_self(args)?;
    if f.is_closed() {
        return Err(value_error("I/O operation on closed file"));
    }
    Ok(Object::File(f))
}

pub(crate) fn file_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `IOBase.__exit__` calls `self.close()`, dispatched through the
    // instance MRO, so a subclass override runs (e.g. test_pathlib's
    // `DummyPathIO.close` that flushes `getvalue()` into a dict on context
    // exit). Mirror that: for a subclass *instance* invoke the resolved
    // `close` (overridden or inherited) via the interpreter; the base native
    // stream closes directly.
    if let Some(self_obj @ Object::Instance(_)) = args.first() {
        if let Some(method) = crate::instance_method(self_obj, "close") {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by an enclosing VM frame live on this thread.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                interp.call_object_with_globals(&method, &[], &[], &globals)?;
                return Ok(Object::None);
            }
        }
    }
    file_self(args)?.close_with_flush()?;
    Ok(Object::None)
}

// ----- memoryview methods (RFC 0023) -----

fn memoryview_self(args: &[Object]) -> Result<Rc<crate::object::PyMemoryView>, RuntimeError> {
    match args.first() {
        Some(Object::MemoryView(mv)) => Ok(mv.clone()),
        _ => Err(type_error("memoryview method requires a memoryview")),
    }
}

/// CPython `memory_richcompare` for EQ: `Some(eq)` when both sides expose a
/// buffer, `None` (→ NotImplemented) when the other side has no buffer or its
/// `bf_getbuffer` refuses the FULL_RO request (a `_testbuffer.ndarray` built
/// with restricted `getbuf=` flags does exactly that). Released views compare
/// by identity, like CPython's `BASE_INACCESSIBLE` branch.
fn memoryview_eq_option(mv: &Rc<crate::object::PyMemoryView>, other: &Object) -> Option<bool> {
    use crate::object::PyMemoryView;
    if mv.released.get() {
        return Some(matches!(other, Object::MemoryView(b) if Rc::ptr_eq(mv, b)));
    }
    match other {
        Object::MemoryView(b) => {
            if b.released.get() {
                return Some(false);
            }
            Some(mv.buffer_eq(b))
        }
        Object::Bytes(b) => {
            let view = PyMemoryView::from_bytes(b.clone());
            Some(mv.buffer_eq(&view))
        }
        Object::ByteArray(b) => {
            let view = PyMemoryView::from_bytearray(b.clone());
            Some(mv.buffer_eq(&view))
        }
        other => {
            if let Some(b) = buffer_exported_view(other) {
                return Some(mv.buffer_eq(&b));
            }
            match crate::foreign::get_buffer_obj(other) {
                Ok(Object::MemoryView(b)) => Some(mv.buffer_eq(&b)),
                _ => None,
            }
        }
    }
}

fn memoryview_tobytes(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error("memoryview: released"));
    }
    // `tobytes(order=None)`: 'C'/None gather row-major, 'F' column-major,
    // 'A' follows the view's own layout ('F' only when Fortran-contiguous).
    let mut order: Option<Object> = args.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "order" => order = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "'{other}' is an invalid keyword argument for tobytes()"
                )))
            }
        }
    }
    let order_ch = match &order {
        None | Some(Object::None) => 'C',
        Some(Object::Str(s)) => match &**s {
            "C" | "F" | "A" => s.chars().next().unwrap(),
            _ => return Err(value_error("order must be 'C', 'F' or 'A'")),
        },
        Some(other) => {
            return Err(type_error(format!(
                "tobytes() argument 'order' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    let fortran = match order_ch {
        'F' => true,
        'A' => mv.is_f_contiguous() && !mv.is_c_contiguous(),
        _ => false,
    };
    if !fortran {
        return Ok(Object::Bytes(Rc::from(mv.to_bytes().into_boxed_slice())));
    }
    // Fortran gather: first index varies fastest.
    let shape = mv.shape_dims();
    let total: usize = shape.iter().product();
    let itemsize = mv.itemsize.get().max(1);
    let mut out = Vec::with_capacity(total * itemsize);
    let mut idx = vec![0usize; shape.len()];
    for _ in 0..total {
        let ok = mv
            .read_element(&idx, |b| -> Result<Object, RuntimeError> {
                out.extend_from_slice(b);
                Ok(Object::None)
            })
            .is_some();
        if !ok {
            return Err(value_error("memoryview: invalid buffer access"));
        }
        for d in 0..shape.len() {
            idx[d] += 1;
            if idx[d] < shape[d] {
                break;
            }
            idx[d] = 0;
        }
    }
    Ok(Object::Bytes(Rc::from(out.into_boxed_slice())))
}

fn memoryview_tolist(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error("memoryview: released"));
    }
    // CPython `memoryview.tolist` unpacks elements per the view's format
    // (`'l'` → ints, `'d'` → floats, …), nesting one list per dimension.
    // Non-native / multi-member formats raise NotImplementedError
    // (`adjust_fmt` parity).
    let shape = mv.shape_dims();
    let fmt = crate::mv_adjust_fmt(&mv.format.borrow())?;
    // Recurse one dimension per level, accumulating the multi-index;
    // `read_element` resolves each leaf (linear- or suboffset-addressed).
    fn build(
        mv: &crate::object::PyMemoryView,
        shape: &[usize],
        prefix: &mut Vec<usize>,
        fmt: char,
    ) -> Result<Object, RuntimeError> {
        if shape.is_empty() {
            return mv
                .read_element(prefix, |b| crate::mv_unpack_single(fmt, b))
                .unwrap_or_else(|| Err(value_error("memoryview: invalid buffer access")));
        }
        let mut out = Vec::with_capacity(shape[0]);
        for i in 0..shape[0] {
            prefix.push(i);
            let item = build(mv, &shape[1..], prefix, fmt);
            prefix.pop();
            out.push(item?);
        }
        Ok(Object::new_list(out))
    }
    build(&mv, &shape, &mut Vec::new(), fmt)
}

fn memoryview_release(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    // CPython `memory_release` refuses while sub-buffers are exported; the
    // hash path holds such an export around the exporter's `__hash__` so a
    // re-entrant release can't free the buffer mid-hash (gh-142664).
    let exports = mv.exports.get();
    if exports > 0 {
        return Err(RuntimeError::PyException(
            crate::error::PyException::from_builtin(
                "BufferError",
                format!(
                    "memoryview has {exports} exported buffer{}",
                    if exports == 1 { "" } else { "s" }
                ),
            ),
        ));
    }
    if mv.released.get() {
        // Idempotent — and the PEP 688 release hook must not re-fire.
        return Ok(Object::None);
    }
    let inner = mv.release_inner.borrow_mut().take();
    let exporter = mv.exporter.borrow().clone();
    mv.release();
    if let Some(inner_obj) = inner {
        pep688_release_hook(&inner_obj, exporter.as_ref())?;
    }
    Ok(Object::None)
}

/// PEP 688 exporter notification (CPython `slot_bf_releasebuffer`): a view
/// built from a Python `__buffer__` hands the *same* memoryview object back
/// to the exporter's `__release_buffer__`. When the exporter's class also
/// carries a native buffer (a `bytearray` subclass), the C base's
/// releasebuffer runs afterwards, so the view is dead once the hook returns
/// (`releasebuffer_maybe_call_super`).
fn pep688_release_hook(inner: &Object, exporter: Option<&Object>) -> Result<(), RuntimeError> {
    let Some(exp @ Object::Instance(inst)) = exporter else {
        return Ok(());
    };
    let Some(hook) = inst.cls().lookup("__release_buffer__") else {
        return Ok(());
    };
    if matches!(hook, Object::Builtin(_)) {
        // Native releasebuffer (plain bytearray & co): drop the export now.
        if let Object::MemoryView(iv) = inner {
            iv.release();
        }
        return Ok(());
    }
    // Python hook: the passed view is export-restricted (CPython's
    // `_Py_MEMORYVIEW_RESTRICTED` — reads work, new exports raise
    // ValueError) and stays so, matching CPython.
    if let Object::MemoryView(iv) = inner {
        iv.restricted.set(true);
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        type_error("__release_buffer__ requires a running interpreter".to_owned())
    })?;
    // SAFETY: published by `publish_interpreter_ptr` from a `&mut
    // Interpreter` still on the call stack; the GIL makes this thread's
    // access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    let res = interp.call(&hook, &[exp.clone(), inner.clone()], &[], &globals);
    if matches!(inst.native.get(), Some(Object::ByteArray(_))) {
        if let Object::MemoryView(iv) = inner {
            iv.release();
        }
    }
    res.map(|_| ())
}

fn memoryview_cast(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() || mv.restricted.get() {
        return Err(value_error(
            "operation forbidden on released memoryview object",
        ));
    }
    // `memoryview.cast(format[, shape])`: `format` is positional-or-keyword and
    // `shape` (optional) reshapes the view for multi-dimensional indexing.
    let mut format: Option<Object> = args.get(1).cloned();
    let mut shape: Option<Object> = args.get(2).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "format" => format = Some(v.clone()),
            "shape" => shape = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "'{other}' is an invalid keyword argument for cast()"
                )));
            }
        }
    }
    if args.len() > 3 {
        return Err(type_error(format!(
            "cast expected at most 2 arguments, got {}",
            args.len() - 1
        )));
    }
    // CPython restricts casts to C-contiguous views (`mv[::2].cast(...)`
    // raises `TypeError: memoryview: casts are restricted to C-contiguous
    // views`).
    if !mv.is_c_contiguous() {
        return Err(type_error(
            "memoryview: casts are restricted to C-contiguous views",
        ));
    }
    // CPython `memory_cast`: a multi-dimensional (or explicitly reshaped)
    // view with a zero in its shape is C-contiguous by the definition above
    // but casts over it are undefined, so they are refused up front.
    let cur_shape = mv.shape_dims();
    if (shape.is_some() || cur_shape.len() != 1) && cur_shape.contains(&0) {
        return Err(type_error(
            "memoryview: cannot cast view with zeros in shape or strides",
        ));
    }
    // Shape argument validation runs before the format checks
    // (CPython `memory_cast_impl` validates `shape` before `cast_to_1D`).
    let shape_items: Option<Vec<Object>> = match &shape {
        None => None,
        Some(Object::List(items)) => Some(items.borrow().clone()),
        Some(Object::Tuple(items)) => Some(items.to_vec()),
        Some(_) => {
            return Err(type_error(
                "memoryview.cast(): shape must be a list or a tuple",
            ))
        }
    };
    if let Some(items) = &shape_items {
        if items.len() > 64 {
            return Err(value_error(
                "memoryview: number of dimensions must not exceed 64",
            ));
        }
        if cur_shape.len() != 1 && items.len() != 1 {
            return Err(type_error("memoryview: cast must be 1D -> ND or ND -> 1D"));
        }
    }
    let Some(Object::Str(fmt)) = &format else {
        return Err(type_error("memoryview: cast format must be a string"));
    };
    // Destination format: an optional '@' followed by one native code
    // (CPython `get_native_fmtchar`). Native itemsizes: 1 for B/b/c/?,
    // 2 for h/H/e, 4 for i/I/f, sizeof(long) for l/L, 8 for q/Q/d/n/N/P.
    let dest_body = fmt.strip_prefix('@').unwrap_or(fmt);
    let bad_dest = || {
        value_error(
            "memoryview: destination format must be a native single character format prefixed with an optional '@'",
        )
    };
    let destchar = {
        let mut it = dest_body.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => c,
            _ => return Err(bad_dest()),
        }
    };
    let itemsize = match destchar {
        'B' | 'b' | 'c' | '?' => 1,
        'h' | 'H' | 'e' => 2,
        'i' | 'I' | 'f' => 4,
        'l' | 'L' => std::mem::size_of::<std::os::raw::c_long>(),
        'q' | 'Q' | 'd' | 'n' | 'N' | 'P' => 8,
        _ => return Err(bad_dest()),
    };
    // CPython `cast_to_1D`: at least one side of the cast must be a byte
    // format (B, b, or c); a non-native source format counts as non-byte.
    let is_byte = |c: char| matches!(c, 'B' | 'b' | 'c');
    let src_byte = {
        let src_fmt = mv.format.borrow();
        let body = src_fmt.strip_prefix('@').unwrap_or(&src_fmt);
        let mut it = body.chars();
        matches!((it.next(), it.next()), (Some(c), None) if is_byte(c))
    };
    if !src_byte && !is_byte(destchar) {
        return Err(type_error(
            "memoryview: cannot cast between two non-byte formats",
        ));
    }
    let nbytes = mv.len.get();
    if nbytes % itemsize != 0 {
        return Err(type_error(
            "memoryview: length is not a multiple of itemsize",
        ));
    }
    // Build the new shape. With no explicit `shape`, cast yields a flat
    // `[nbytes / itemsize]` 1-D view; an explicit shape must agree on size.
    let dims: Vec<usize> = match shape_items {
        None => vec![nbytes / itemsize],
        Some(items) => {
            let mut dims = Vec::with_capacity(items.len());
            let mut prod: usize = 1;
            for d in &items {
                // CPython `cast_to_ND`: only real ints pass (TypeError),
                // `PyLong_AsSsize_t` overflow propagates OverflowError,
                // non-positive dims and a product exceeding SSIZE_MAX are
                // ValueErrors.
                let n = match d {
                    Object::Bool(b) => i64::from(*b),
                    Object::Int(n) => *n,
                    Object::Long(b) => b.to_i64().ok_or_else(|| {
                        crate::error::overflow_error("Python int too large to convert to C ssize_t")
                    })?,
                    _ => {
                        return Err(type_error(
                            "memoryview.cast(): elements of shape must be integers",
                        ))
                    }
                };
                if n <= 0 {
                    return Err(value_error(
                        "memoryview.cast(): elements of shape must be integers > 0",
                    ));
                }
                prod = prod
                    .checked_mul(n as usize)
                    .filter(|&p| isize::try_from(p).is_ok())
                    .ok_or_else(|| value_error("memoryview.cast(): product(shape) > SSIZE_MAX"))?;
                dims.push(n as usize);
            }
            let total = prod
                .checked_mul(itemsize)
                .filter(|&t| isize::try_from(t).is_ok())
                .ok_or_else(|| {
                    value_error("memoryview.cast(): product(shape) * itemsize > SSIZE_MAX")
                })?;
            if total != nbytes {
                return Err(type_error(
                    "memoryview: product(shape) * itemsize != buffer size",
                ));
            }
            dims
        }
    };
    // A fresh view over the *same* buffer (shares the export); the original
    // `mv` is left untouched, matching CPython's non-mutating `cast`.
    let cast = mv.shallow_clone();
    let zero_dim = dims.is_empty();
    *cast.format.borrow_mut() = fmt.to_string();
    cast.itemsize.set(itemsize);
    *cast.strides.borrow_mut() = crate::object::c_contiguous_strides(&dims, itemsize);
    *cast.shape.borrow_mut() = dims;
    // `ndim` becomes `len(shape)` (CPython `memory_cast`): casting a 0-dim
    // view (a ctypes Structure export) to a flat format yields a regular
    // 1-D view so `mv.cast('B')[:] = data` works (test_io byteslike), and
    // an explicit empty shape (`m.cast('I', shape=())`) yields a 0-dim
    // scalar view (test_buffer.test_memoryview_cast).
    cast.zero_dim.set(zero_dim);
    Ok(Object::MemoryView(Rc::new(cast)))
}

/// `memoryview.hex([sep[, bytes_per_sep]])` — shares `bytes.hex`'s full
/// separator/grouping logic. The view is pinned (an export) for the
/// duration: a `sep.__len__` that re-entrantly `release()`s the view gets
/// a BufferError instead of a use-after-free (gh-143195,
/// test_memoryview.test_hex_use_after_free).
fn memoryview_hex(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error(
            "operation forbidden on released memoryview object",
        ));
    }
    mv.exports.set(mv.exports.get() + 1);
    let r = bytes_hex_kw(args, kwargs);
    mv.exports.set(mv.exports.get() - 1);
    r
}

fn memoryview_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    // `with m:` on a released view refuses up front (CPython
    // `memory_enter` runs CHECK_RELEASED;
    // test_memoryview._check_released).
    if let Some(Object::MemoryView(mv)) = args.first() {
        if mv.released.get() {
            return Err(value_error(
                "operation forbidden on released memoryview object",
            ));
        }
    }
    Ok(args[0].clone())
}

fn memoryview_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    // Leaving the `with` block releases the view (CPython `memory_exit` →
    // `_memory_release`; test_memoryview.test_contextmanager asserts every
    // operation refuses afterwards). Release is idempotent, so an explicit
    // `m.release()` inside the block is fine.
    memoryview_release(&args[..1])?;
    Ok(Object::None)
}

/// `memoryview.toreadonly()` — a fresh view over the same buffer with
/// the readonly bit set (CPython `memory_toreadonly`).
fn memoryview_toreadonly(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() || mv.restricted.get() {
        return Err(value_error(
            "operation forbidden on released memoryview object",
        ));
    }
    let ro = mv.shallow_clone();
    ro.readonly.set(true);
    Ok(Object::MemoryView(Rc::new(ro)))
}

/// `memoryview.__iter__` — iterate the unpacked elements of a 1-D view
/// (CPython `memory_iter` via the sequence protocol).
fn memoryview_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error(
            "operation forbidden on released memoryview object",
        ));
    }
    let items = crate::mv_element_objects(&mv)?;
    Ok(Object::Iter(Rc::new(crate::sync::RefCell::new(
        crate::object::PyIterator::Tuple {
            items: Rc::from(items),
            index: 0,
        },
    ))))
}

/// `staticmethod.__call__(self, *args, **kwargs)` — invoke the wrapped
/// callable directly (bpo-43682, 3.10+).
fn staticmethod_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(Object::StaticMethod(w)) = args.first() else {
        return Err(type_error("__call__() requires a staticmethod receiver"));
    };
    let func = w.func();
    let interp = reentrant_interp()?;
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(&func, &args[1..], kwargs, &globals)
}

/// Fetch the interpreter published by the enclosing VM frame — the
/// subscript slot methods below delegate to the VM's own subscript
/// machinery so their behavior is byte-for-byte `recv[key]`.
pub(crate) fn reentrant_interp() -> Result<&'static mut crate::Interpreter, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: the pointer was published by an enclosing VM frame still
    // live on this thread; the GIL keeps the access exclusive.
    Ok(unsafe { &mut *ptr })
}

/// `recv.__getitem__(key)` for native buffer types — same code path as
/// the `BINARY_SUBSCR` opcode (RFC 0056 WS4).
fn reentrant_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, key] = args else {
        return Err(type_error(format!(
            "__getitem__() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    reentrant_interp()?.binary_subscr(recv, key)
}

/// `recv.__setitem__(key, value)` — same code path as `STORE_SUBSCR`.
fn reentrant_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, key, value] = args else {
        return Err(type_error(format!(
            "__setitem__() takes exactly 2 arguments ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    let interp = reentrant_interp()?;
    let globals = interp.builtins_dict();
    interp.store_subscr(recv, key, value.clone(), &globals)?;
    Ok(Object::None)
}

/// `recv.__delitem__(key)` — same code path as `DELETE_SUBSCR`.
fn reentrant_delitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, key] = args else {
        return Err(type_error(format!(
            "__delitem__() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    reentrant_interp()?.delete_subscr(recv, key)?;
    Ok(Object::None)
}

/// `bytearray.__iadd__(other)` — in-place extend with any bytes-like,
/// returning the receiver (CPython `bytearray_iconcat`).
fn bytearray_iadd(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__iadd__() missing self"))?;
    let ba = bytearray_self(args)?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("__iadd__() takes exactly one argument (0 given)"))?;
    let extra = match other {
        Object::Bytes(b) => b.to_vec(),
        // Cloning first makes `b += b` safe (no aliasing borrow).
        Object::ByteArray(b) => b.borrow().clone(),
        Object::MemoryView(mv) => mv.to_bytes(),
        _ => {
            return Err(type_error(format!(
                "can't concat {} to bytearray",
                other.type_name()
            )));
        }
    };
    ba.borrow_mut().extend_from_slice(&extra);
    Ok(recv)
}

/// `bytearray.__imul__(n)` — in-place repeat, returning the receiver
/// (CPython `bytearray_irepeat`).
fn bytearray_imul(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__imul__() missing self"))?;
    let ba = bytearray_self(args)?;
    let n = args.get(1).and_then(|o| o.as_i64()).ok_or_else(|| {
        type_error(format!(
            "can't multiply sequence by non-int of type '{}'",
            args.get(1).map_or("NoneType", |o| o.type_name())
        ))
    })?;
    let mut data = ba.borrow_mut();
    if n <= 0 {
        data.clear();
    } else {
        let once = data.clone();
        for _ in 1..n {
            data.extend_from_slice(&once);
        }
    }
    drop(data);
    Ok(recv)
}

// ----- dict view + mappingproxy methods (RFC 0023) -----

/// Re-key a `mappingproxy` receiver as the wrapped dict so the dict
/// method implementations can be reused verbatim (the proxy is a
/// read-only *view*, so the share is intentional).
fn mappingproxy_args(args: &[Object]) -> Vec<Object> {
    let mut v = args.to_vec();
    if let Some(Object::MappingProxy(d)) = v.first() {
        v[0] = Object::Dict(d.clone());
    }
    v
}

/// Delegate a method call on an *object-backed* `mappingproxy` to the
/// wrapped mapping itself — CPython's `mappingproxy_*` C methods all
/// call straight through, so a dict subclass's overridden `keys()` /
/// `get()` / `copy()` shows in the view (test_types test_customdict).
fn mappingproxy_delegate(args: &[Object], method: &str) -> Option<Result<Object, RuntimeError>> {
    let Some(Object::MappingProxyObj(inner)) = args.first() else {
        return None;
    };
    let inner = (**inner).clone();
    let rest = args[1..].to_vec();
    Some((|| {
        let interp = reentrant_interp()?;
        let m = interp.load_attr_public(&inner, method)?;
        let g = interp.builtins_dict();
        interp.call(&m, &rest, &[], &g)
    })())
}

fn mappingproxy_get(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "get") {
        return r;
    }
    dict_get(&mappingproxy_args(args))
}

fn mappingproxy_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "keys") {
        return r;
    }
    dict_keys(&mappingproxy_args(args))
}

fn mappingproxy_values(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "values") {
        return r;
    }
    dict_values(&mappingproxy_args(args))
}

fn mappingproxy_items(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "items") {
        return r;
    }
    dict_items(&mappingproxy_args(args))
}

fn mappingproxy_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "copy") {
        return r;
    }
    dict_copy(&mappingproxy_args(args))
}

fn mappingproxy_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::MappingProxyObj(_)) = args.first() {
        // Full subscript dispatch (`__missing__` on a dict subclass
        // must fire — test_types test_missing).
        let key = args
            .get(1)
            .cloned()
            .ok_or_else(|| type_error("__getitem__() expected an argument"))?;
        let recv = args[0].clone();
        let interp = reentrant_interp()?;
        return interp.subscr_get_public(&recv, &key);
    }
    dict_getitem(&mappingproxy_args(args))
}

fn mappingproxy_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__iter__() missing self"))?;
    let interp = reentrant_interp()?;
    let g = interp.builtins_dict();
    interp.make_iter(&recv, &g)
}

fn mappingproxy_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = mappingproxy_delegate(args, "__reversed__") {
        return r;
    }
    b_reversed(&mappingproxy_args(args))
}

fn mappingproxy_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__len__() missing self"))?;
    if matches!(recv, Object::MappingProxyObj(_)) {
        let interp = reentrant_interp()?;
        let g = interp.builtins_dict();
        return interp.do_len_call(&recv, &g);
    }
    obj_len(args)
}

fn mappingproxy_contains(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__contains__() missing self"))?;
    let item = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("__contains__() takes exactly one argument (0 given)"))?;
    if matches!(recv, Object::MappingProxyObj(_)) {
        let interp = reentrant_interp()?;
        return Ok(Object::Bool(interp.py_contains(&recv, &item)?));
    }
    obj_contains(args)
}

fn mappingproxy_or(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = match args {
        [a, b] => (a.clone(), b.clone()),
        _ => return Err(type_error("__or__() expected an argument")),
    };
    let interp = reentrant_interp()?;
    interp.op_binary(&a, &b, weavepy_compiler::BinOpKind::BitOr)
}

fn mappingproxy_ror(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = match args {
        [a, b] => (a.clone(), b.clone()),
        _ => return Err(type_error("__ror__() expected an argument")),
    };
    let interp = reentrant_interp()?;
    interp.op_binary(&b, &a, weavepy_compiler::BinOpKind::BitOr)
}

fn mappingproxy_ior(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error(
        "'|=' is not supported by mappingproxy; use '|' instead",
    ))
}

/// The attribute-namespace dict of a `SimpleNamespace` (or an instance
/// of a SimpleNamespace subclass, whose instance `__dict__` *is* the
/// namespace — see the instantiation path in lib.rs).
fn ns_dict_of(recv: &Object) -> Option<Rc<RefCell<crate::object::DictData>>> {
    match recv {
        Object::SimpleNamespace(d) => Some(d.clone()),
        Object::Instance(i) if matches!(i.native.get(), Some(Object::SimpleNamespace(_))) => {
            Some(i.dict.clone())
        }
        _ => None,
    }
}

/// The class object of a namespace receiver (`types.SimpleNamespace`
/// itself for the plain variant, the subclass for instances).
fn ns_class_of(recv: &Object) -> Object {
    match recv {
        Object::Instance(i) => Object::Type(i.cls()),
        _ => Object::Type(
            crate::builtin_types::builtin_types()
                .simple_namespace_
                .clone(),
        ),
    }
}

/// CPython `namespace_reduce`: `(type(self), (), self.__dict__)`. Also
/// serves as `__reduce_ex__` (the protocol argument changes nothing),
/// which makes every pickle protocol work (test_types
/// SimpleNamespaceTests.test_pickle).
pub(crate) fn namespace_reduce(args: &[Object]) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__reduce__() missing self"))?;
    let d = ns_dict_of(recv)
        .ok_or_else(|| type_error(format!("cannot pickle '{}' object", recv.type_name())))?;
    // A fresh dict object, not the live `ns_dict` Rc: the namespace and
    // its dict would otherwise share one object id, and pickle's memo
    // would resolve the state back to the namespace itself.
    let snapshot: crate::object::DictData = d
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Object::new_tuple(vec![
        ns_class_of(recv),
        Object::new_tuple(vec![]),
        Object::Dict(Rc::new(RefCell::new(snapshot))),
    ]))
}

/// CPython `namespace_replace` (`__replace__`, 3.13): build a fresh
/// instance via `type(self)()`, verify it *is* a namespace (gh-143636:
/// a rogue `__new__` may return anything), then lay down the current
/// attributes and the keyword changes.
pub(crate) fn namespace_replace(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let recv = args
        .first()
        .ok_or_else(|| type_error("__replace__() missing self"))?;
    if args.len() > 1 {
        return Err(type_error("__replace__() takes no positional arguments"));
    }
    let self_dict = ns_dict_of(recv).ok_or_else(|| {
        type_error(format!(
            "descriptor '__replace__' requires a 'types.SimpleNamespace' object but received a '{}'",
            recv.type_name()
        ))
    })?;
    let cls = ns_class_of(recv);
    let interp = reentrant_interp()?;
    let g = interp.builtins_dict();
    let new = interp.call(&cls, &[], &[], &g)?;
    let Some(new_dict) = ns_dict_of(&new) else {
        // Module-qualified class name, as CPython renders it.
        let cls_name = match &cls {
            Object::Type(t) => {
                let module = t.lookup("__module__").and_then(|m| match m {
                    Object::Str(s) => Some(s.to_string()),
                    _ => None,
                });
                let qual = t
                    .lookup("__qualname__")
                    .and_then(|m| match m {
                        Object::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| t.name.clone());
                match module {
                    Some(m) => format!("{m}.{qual}"),
                    None => qual,
                }
            }
            other => other.type_name().to_owned(),
        };
        return Err(type_error(format!(
            "expect types.SimpleNamespace type, but {}() returned '{}' object",
            cls_name,
            new.type_name()
        )));
    };
    let entries: Vec<(DictKey, Object)> = self_dict
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    {
        let mut nd = new_dict.borrow_mut();
        for (k, v) in entries {
            nd.insert(k, v);
        }
        for (k, v) in kwargs {
            nd.insert(DictKey(Object::from_str(k.clone())), v.clone());
        }
    }
    Ok(new)
}

/// Install the full CPython `mappingproxy` method surface into the
/// builtin type's dict (RFC 0068 WS5) — `dir(mappingproxy({}))` minus
/// `dir(object())` is asserted verbatim by test_types.test_methods.
pub(crate) fn install_mappingproxy_methods(ty: &Rc<crate::types::TypeObject>) {
    use crate::object::{BuiltinFn, DictKey};
    let entries: &[(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)] = &[
        ("__contains__", mappingproxy_contains),
        ("__getitem__", mappingproxy_getitem),
        ("__iter__", mappingproxy_iter),
        ("__len__", mappingproxy_len),
        ("__or__", mappingproxy_or),
        ("__ror__", mappingproxy_ror),
        ("__ior__", mappingproxy_ior),
        ("__reversed__", mappingproxy_reversed),
        ("copy", mappingproxy_copy),
        ("get", mappingproxy_get),
        ("items", mappingproxy_items),
        ("keys", mappingproxy_keys),
        ("values", mappingproxy_values),
    ];
    let mut dict = ty.dict.borrow_mut();
    for (name, f) in entries {
        dict.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(*f),
                call_kw: None,
            })),
        );
    }
    // PEP 585: `mappingproxy[str, int]` — a classmethod minting a
    // `types.GenericAlias` (CPython `Py_GenericAliasCC`).
    fn class_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
        let cls = args
            .first()
            .cloned()
            .ok_or_else(|| type_error("__class_getitem__() missing class"))?;
        let item = args
            .get(1)
            .cloned()
            .ok_or_else(|| type_error("__class_getitem__() expected an argument"))?;
        Ok(crate::make_generic_alias_public(cls, item))
    }
    dict.insert(
        DictKey(Object::from_static("__class_getitem__")),
        Object::ClassMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
            BuiltinFn {
                name: "__class_getitem__",
                binds_instance: false,
                call: Box::new(class_getitem),
                call_kw: None,
            },
        )))),
    );
}

fn view_isdisjoint(args: &[Object]) -> Result<Object, RuntimeError> {
    let other = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("isdisjoint() expected an argument"))?;
    let mut other_iter = other.make_iter()?;
    // `DictKey` is hashable like CPython's dict keys; the inner Rcs are
    // borrowed read-only during hashing, so the mutable-key-type lint
    // doesn't apply.
    #[allow(clippy::mutable_key_type)]
    let mut other_set = std::collections::HashSet::new();
    while let Some(v) = other_iter.next_value() {
        other_set.insert(crate::object::DictKey(v));
    }
    let mut self_iter = args[0].make_iter()?;
    while let Some(v) = self_iter.next_value() {
        if other_set.contains(&crate::object::DictKey(v)) {
            return Ok(Object::Bool(false));
        }
    }
    Ok(Object::Bool(true))
}
