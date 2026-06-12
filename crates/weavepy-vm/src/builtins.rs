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
    index_error, key_error, runtime_error, stop_iteration, type_error, value_error, RuntimeError,
};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyIterator, Range};

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
        "str" => ctor!("str", b_str),
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
        _ => None,
    }
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
    Ok(Object::Slice(Rc::new(crate::object::PySlice { start, stop, step })))
}

/// Build the dict that backs the `builtins` module.
pub fn default_builtins() -> DictData {
    let mut d = DictData::new();
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

    reg!("len", b_len);
    reg!("range", b_range);
    reg!("str", b_str);
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
    reg!("enumerate", b_enumerate);
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
    reg!("round", b_round);
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
    reg!("pow", b_pow);
    reg!("breakpoint", b_breakpoint);
    reg!("memoryview", b_memoryview);
    reg!("__weavepy_typevar__", b_typevar);
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
            "casefold" => Some(method("casefold", str_lower)),
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
            "count" => Some(method("count", str_count)),
            "partition" => Some(method("partition", str_partition)),
            "rpartition" => Some(method("rpartition", str_rpartition)),
            "isdigit" => Some(method("isdigit", str_isdigit)),
            "isalpha" => Some(method("isalpha", str_isalpha)),
            "isalnum" => Some(method("isalnum", str_isalnum)),
            "isspace" => Some(method("isspace", str_isspace)),
            "isupper" => Some(method("isupper", str_isupper)),
            "islower" => Some(method("islower", str_islower)),
            "isascii" => Some(method("isascii", str_isascii)),
            "isnumeric" => Some(method("isnumeric", str_isdigit)),
            "isdecimal" => Some(method("isdecimal", str_isdigit)),
            "isidentifier" => Some(method("isidentifier", str_isidentifier)),
            "isprintable" => Some(method("isprintable", str_isprintable)),
            "zfill" => Some(method("zfill", str_zfill)),
            "ljust" => Some(method("ljust", str_ljust)),
            "rjust" => Some(method("rjust", str_rjust)),
            "center" => Some(method("center", str_center)),
            "expandtabs" => Some(method("expandtabs", str_expandtabs)),
            "encode" => Some(method("encode", str_encode)),
            "removeprefix" => Some(method("removeprefix", str_removeprefix)),
            "removesuffix" => Some(method("removesuffix", str_removesuffix)),
            "format" => Some(method(".format", str_format)),
            "format_map" => Some(method(".format_map", str_format_map)),
            "translate" => Some(method("translate", str_translate)),
            "maketrans" => Some(method("maketrans", str_maketrans)),
            // Sequence dunders so `hasattr(s, '__getitem__')` and direct
            // `str.__getitem__(s, i)` calls work (CPython exposes these as
            // slot wrappers; `operator.concat` probes `__getitem__`).
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
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
            "fromkeys" => Some(method("fromkeys", dict_fromkeys)),
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
            _ => None,
        },
        Object::Tuple(_) => match name {
            "count" => Some(method("count", tuple_count)),
            "index" => Some(method("index", tuple_index)),
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            _ => None,
        },
        Object::Set(_) | Object::FrozenSet(_) => match name {
            "add" => Some(method("add", set_add)),
            "discard" => Some(method("discard", set_discard)),
            "remove" => Some(method("remove", set_remove)),
            "pop" => Some(method("pop", set_pop)),
            "clear" => Some(method("clear", set_clear)),
            "copy" => Some(method("copy", set_copy)),
            "update" => Some(method("update", set_update)),
            "union" => Some(method("union", set_union)),
            "intersection" => Some(method("intersection", set_intersection)),
            "difference" => Some(method("difference", set_difference)),
            "symmetric_difference" => {
                Some(method("symmetric_difference", set_symmetric_difference))
            }
            "issubset" => Some(method("issubset", set_issubset)),
            "issuperset" => Some(method("issuperset", set_issuperset)),
            "isdisjoint" => Some(method("isdisjoint", set_isdisjoint)),
            "intersection_update" => Some(method("intersection_update", set_intersection_update)),
            "difference_update" => Some(method("difference_update", set_difference_update)),
            "symmetric_difference_update" => Some(method(
                "symmetric_difference_update",
                set_symmetric_difference_update,
            )),
            // Membership dunder exposed as a bound method: CPython's
            // `keyword.iskeyword = frozenset(kwlist).__contains__` grabs it
            // directly, and `hasattr(s, '__contains__')` must hold.
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__len__" => Some(method("__len__", obj_len)),
            _ => None,
        },
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
            "maketrans" => Some(method("maketrans", bytes_maketrans)),
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
            "copy" if matches!(obj, Object::ByteArray(_)) => {
                Some(method("copy", bytearray_copy))
            }
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
            // Sequence dunders so direct calls / `hasattr` parity hold.
            "__contains__" => Some(method("__contains__", obj_contains)),
            "__len__" => Some(method("__len__", obj_len)),
            "__getitem__" => Some(method("__getitem__", seq_getitem)),
            // PEP 461 `%`-formatting exposed as the number-protocol
            // dunders (`bytes_mod` fills CPython's `nb_remainder` slot,
            // so both wrappers exist).
            "__mod__" => Some(method("__mod__", bytes_dunder_mod)),
            "__rmod__" => Some(method("__rmod__", bytes_dunder_rmod)),
            "__bytes__" if matches!(obj, Object::Bytes(_)) => {
                Some(method("__bytes__", |args| {
                    match args.first() {
                        Some(Object::Bytes(b)) => Ok(Object::Bytes(b.clone())),
                        _ => Err(type_error("__bytes__ requires a bytes receiver")),
                    }
                }))
            }
            _ => None,
        },
        Object::File(_) => match name {
            "read" => Some(method("read", file_read)),
            "readline" => Some(method("readline", file_readline)),
            "readlines" => Some(method("readlines", file_readlines)),
            "write" => Some(method("write", file_write)),
            // Routed through the interpreter (sentinel name) so it can
            // consume *any* iterable via the full `__iter__`/`__next__`
            // protocol, not just native sequences.
            "writelines" => Some(method(".file_writelines", file_writelines)),
            "flush" => Some(method("flush", file_flush)),
            "close" => Some(method("close", file_close)),
            "seek" => Some(method("seek", file_seek)),
            "tell" => Some(method("tell", file_tell)),
            "getvalue" => Some(method("getvalue", file_getvalue)),
            "__enter__" => Some(method("__enter__", file_enter)),
            "__exit__" => Some(method("__exit__", file_exit)),
            // A file is its own iterator (CPython): `iter(f) is f`, and
            // each `next(f)` returns the next line, raising StopIteration
            // at EOF.
            "__iter__" => Some(method("__iter__", |args| {
                file_self(args).map(Object::File)
            })),
            "__next__" => Some(method("__next__", file_next)),
            _ => None,
        },
        Object::MemoryView(_) => match name {
            "tobytes" => Some(method("tobytes", memoryview_tobytes)),
            "tolist" => Some(method("tolist", memoryview_tolist)),
            "release" => Some(method("release", memoryview_release)),
            "cast" => Some(method("cast", memoryview_cast)),
            "hex" => Some(method("hex", memoryview_hex)),
            "__enter__" => Some(method("__enter__", memoryview_enter)),
            "__exit__" => Some(method("__exit__", memoryview_exit)),
            _ => None,
        },
        Object::DictView(_) => match name {
            "isdisjoint" => Some(method("isdisjoint", view_isdisjoint)),
            "mapping" => None,
            _ => None,
        },
        // `mappingproxy` (read-only `type.__dict__` view) forwards the
        // read-side mapping API to the wrapped dict.
        Object::MappingProxy(_) => match name {
            "isdisjoint" => Some(method("isdisjoint", view_isdisjoint)),
            "get" => Some(method("get", mappingproxy_get)),
            "keys" => Some(method("keys", mappingproxy_keys)),
            "values" => Some(method("values", mappingproxy_values)),
            "items" => Some(method("items", mappingproxy_items)),
            "copy" => Some(method("copy", mappingproxy_copy)),
            "__getitem__" => Some(method("__getitem__", mappingproxy_getitem)),
            "__len__" => Some(method("__len__", obj_len)),
            "__contains__" => Some(method("__contains__", obj_contains)),
            _ => None,
        },
        Object::SimpleNamespace(_) => match name {
            "__repr__" => None,
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
            "__get__" => Some(method("__get__", property_dunder_get)),
            "__set__" => Some(method("__set__", property_dunder_set)),
            "__delete__" => Some(method("__delete__", property_dunder_delete)),
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
            _ => None,
        },
        Object::ClassMethod(_) => match name {
            "__get__" => Some(method("__get__", classmethod_descr_get)),
            _ => None,
        },
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => match name {
            "bit_length" => Some(method("bit_length", int_bit_length)),
            "bit_count" => Some(method("bit_count", int_bit_count)),
            "to_bytes" => Some(method("to_bytes", int_to_bytes)),
            "from_bytes" => Some(method("from_bytes", int_from_bytes_method)),
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
            _ => numeric_dunder(obj, name),
        },
        Object::Float(_) => match name {
            "is_integer" => Some(method("is_integer", float_is_integer)),
            "hex" => Some(method("hex", float_hex)),
            "fromhex" => Some(method("fromhex", float_fromhex)),
            "as_integer_ratio" => Some(method("as_integer_ratio", float_as_integer_ratio)),
            "conjugate" => Some(method("conjugate", float_conjugate)),
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
            // Pickling support. The actual reduction needs the canonical
            // `iter` builtin (so the result pickles by name and round-trips),
            // which requires interpreter access — the VM intercepts this
            // sentinel name in its bound-method dispatch.
            "__reduce__" => Some(method(".iter_reduce", |_| {
                Err(type_error("iterator.__reduce__ requires the interpreter"))
            })),
            _ => None,
        },
        _ => None,
    };
    f.map(|f| Object::Builtin(Rc::new(f)))
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
            Object::ByteArray(b) => b.borrow().iter().map(|x| Object::Int(i64::from(*x))).collect(),
            _ => Vec::new(),
        }
    };
    match index {
        Object::Slice(s) => {
            let seq = as_seq(recv);
            let sliced = crate::slice_seq(&seq, s)?;
            Ok(match recv {
                Object::Str(_) => Object::from_str(sliced.iter().map(Object::to_str).collect::<String>()),
                Object::Tuple(_) => Object::new_tuple(sliced),
                Object::Bytes(_) => {
                    let bytes: Vec<u8> = sliced.iter().filter_map(|o| o.as_i64()).map(|i| i as u8).collect();
                    Object::new_bytes(bytes)
                }
                Object::ByteArray(_) => {
                    let bytes: Vec<u8> = sliced.iter().filter_map(|o| o.as_i64()).map(|i| i as u8).collect();
                    Object::new_bytearray(bytes)
                }
                _ => Object::new_list(sliced),
            })
        }
        _ => {
            let i = coerce_index_i64(index)?;
            let seq = as_seq(recv);
            let idx = crate::normalize_index(i, seq.len())?;
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
    let length = match args.get(1) {
        Some(o) => coerce_index_i64(o)?,
        None => {
            return Err(type_error(
                "indices() takes exactly one argument (0 given)",
            ))
        }
    };
    if length < 0 {
        return Err(value_error("length should not be negative"));
    }
    let step = match &s.step {
        Object::None => 1,
        o => {
            let st = coerce_index_i64(o)?;
            if st == 0 {
                return Err(value_error("slice step cannot be zero"));
            }
            st
        }
    };
    let (lower, upper) = if step < 0 {
        (-1i64, length - 1)
    } else {
        (0i64, length)
    };
    let clamp = |v: i64| -> i64 {
        if v < 0 {
            (v + length).max(lower)
        } else {
            v.min(upper)
        }
    };
    let start = match &s.start {
        Object::None => {
            if step < 0 {
                upper
            } else {
                lower
            }
        }
        o => clamp(coerce_index_i64(o)?),
    };
    let stop = match &s.stop {
        Object::None => {
            if step < 0 {
                lower
            } else {
                upper
            }
        }
        o => clamp(coerce_index_i64(o)?),
    };
    Ok(Object::new_tuple(vec![
        Object::Int(start),
        Object::Int(stop),
        Object::Int(step),
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
        crate::unary_op(&s, op)
    })
}

/// `(value).__getnewargs__()` for the built-in numerics: `complex`
/// reconstructs from `(real, imag)`, the rest from `(value,)`.
fn num_getnewargs(self_o: &Object) -> Object {
    let native = self_o.native_value();
    let v = native.as_ref().unwrap_or(self_o);
    match v {
        Object::Complex(c) => {
            Object::new_tuple(vec![Object::Float(c.real), Object::Float(c.imag)])
        }
        other => Object::new_tuple(vec![other.clone()]),
    }
}

/// Resolve a numeric slot-wrapper dunder by name for receiver `self_repr`.
/// Returns `None` for anything that isn't a numeric dunder so the caller
/// falls through to its other attribute paths.
fn numeric_dunder(self_repr: &Object, name: &str) -> Option<BuiltinFn> {
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
        "__pow__" => num_binop_method("__pow__", kind, B::Pow, false),
        "__rpow__" => num_binop_method("__rpow__", kind, B::Pow, true),
        // `floordiv`/`mod` are undefined on `complex`.
        "__floordiv__" if not_complex => num_binop_method("__floordiv__", kind, B::FloorDiv, false),
        "__rfloordiv__" if not_complex => {
            num_binop_method("__rfloordiv__", kind, B::FloorDiv, true)
        }
        "__mod__" if not_complex => num_binop_method("__mod__", kind, B::Mod, false),
        "__rmod__" if not_complex => num_binop_method("__rmod__", kind, B::Mod, true),
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
        Some(Object::Instance(inst)) => inst.native.clone(),
        other => other.cloned(),
    };
    match native {
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
        ("int", "from_bytes") => Some(method("from_bytes", int_from_bytes_method)),
        ("float", "fromhex") => Some(method("fromhex", float_fromhex)),
        ("dict", "fromkeys") => Some(method("fromkeys", dict_fromkeys)),
        _ => None,
    };
    f.map(|f| Object::Builtin(Rc::new(f)))
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
        "staticmethod" => Object::StaticMethod(Rc::new(Object::None)),
        "classmethod" => Object::ClassMethod(Rc::new(Object::None)),
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
        Some(Object::None) | None => String::new(),
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
        return interp
            .stringify_public(o, &globals)
            .map(Object::from_str);
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
    // present on `type`, functions, methods — not on `object`).
    if name == "__call__"
        && matches!(
            base_name,
            "type" | "function" | "builtin_function_or_method" | "method" | "method-wrapper"
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
        if matches!(base_name, "object" | "str") {
            return Some(Object::Builtin(Rc::new(method("__str__", slot_str))));
        }
        return None;
    }
    let (static_name, f): (&'static str, fn(&[Object]) -> Result<Object, RuntimeError>) =
        match name {
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
        Object::Instance(inst) => 16 + 8 * inst.dict.borrow().len() as i64,
        Object::Str(s) => 49 + s.len() as i64,
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
        let dict_state = if inst.dict.borrow().is_empty() {
            Object::None
        } else {
            Object::Dict(inst.dict.clone())
        };
        if !slots.is_empty() {
            let mut slot_dict = crate::object::DictData::new();
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

/// Coerce `o` to an `i64` index the way CPython's `__index__` protocol does:
/// accept ints/bools directly, unwrap integer-backed subclass instances
/// (e.g. `IntEnum` members), and otherwise invoke a Python-level `__index__`
/// via reentry into the running interpreter. Shared by the integer-position
/// builtins (`range`, slicing helpers, …) so they all honour `__index__`.
/// `coerce_index_i64` widened to `i128` for consumers (like `range`)
/// that must accept bounds beyond the machine-int span. Ints past i128
/// get the CPython-style overflow complaint rather than silent clamping.
pub(crate) fn coerce_index_i128(o: &Object) -> Result<i128, RuntimeError> {
    use num_traits::ToPrimitive;
    match o {
        Object::Bool(b) => return Ok(i128::from(*b)),
        Object::Int(i) => return Ok(i128::from(*i)),
        Object::Long(b) => {
            return b.to_i128().ok_or_else(|| {
                crate::error::overflow_error(
                    "Python int too large to convert to C ssize_t",
                )
            })
        }
        _ => {}
    }
    coerce_index_i64(o).map(i128::from)
}

pub(crate) fn coerce_index_i64(o: &Object) -> Result<i64, RuntimeError> {
    if let Some(v) = o.as_i64() {
        return Ok(v);
    }
    if let Object::Instance(_) = o {
        if let Some(method) = crate::instance_method(o, "__index__") {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: the pointer was published by an enclosing VM frame
                // still live on this thread; the GIL keeps the access exclusive.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                if let Some(v) = r.as_i64() {
                    return Ok(v);
                }
            }
        }
    }
    Err(type_error(format!(
        "'{}' object cannot be interpreted as an integer",
        o.type_name()
    )))
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
            Ok(Some(b.to_f64().unwrap_or(f64::INFINITY)))
        }
        Object::Instance(inst) => {
            if let Some(native) = &inst.native {
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
                        let r =
                            interp.call_object_with_globals(&method, &[], &[], &globals)?;
                        return coerce_f64_opt(&r);
                    }
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn b_range(args: &[Object]) -> Result<Object, RuntimeError> {
    let to_int = |o: &Object| -> Result<i128, RuntimeError> { coerce_index_i128(o) };
    let (start, stop, step) = match args.len() {
        1 => (0, to_int(&args[0])?, 1),
        2 => (to_int(&args[0])?, to_int(&args[1])?, 1),
        3 => (to_int(&args[0])?, to_int(&args[1])?, to_int(&args[2])?),
        n => {
            return Err(type_error(format!(
                "range expected 1 to 3 arguments, got {n}"
            )))
        }
    };
    if step == 0 {
        return Err(value_error("range() arg 3 must not be zero"));
    }
    Ok(Object::Range(Rc::new(Range { start, stop, step })))
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
    if let Object::Long(b) = &args[0] {
        long_str_limit_check(b)?;
    }
    // `str(object, encoding[, errors])` decodes a bytes-like object,
    // equivalent to `object.decode(encoding, errors)`. CPython's
    // `re._parser.Tokenizer` relies on `str(pattern, 'latin1')` to
    // tokenize bytes patterns, so this path must decode rather than
    // fall back to `repr`-style stringification.
    if args.len() >= 2 {
        match &args[0] {
            Object::Bytes(_) | Object::ByteArray(_) => {}
            other => {
                return Err(type_error(format!(
                    "decoding to str: need a bytes-like object, {} found",
                    other.type_name()
                )));
            }
        }
        let data = bytes_data(args)?;
        let encoding = match &args[1] {
            Object::Str(e) => e.to_string(),
            Object::None => "utf-8".to_owned(),
            _ => return Err(type_error("str() argument 'encoding' must be str")),
        };
        let errors = match args.get(2) {
            Some(Object::Str(e)) => e.to_string(),
            Some(Object::None) | None => "strict".to_owned(),
            _ => return Err(type_error("str() argument 'errors' must be str")),
        };
        let s = crate::stdlib::codecs_mod::decode_bytes(&data, &encoding, &errors)?;
        return Ok(Object::from_str(s));
    }
    Ok(Object::from_str(args[0].to_str()))
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
    Ok(Object::Property(Rc::new(crate::object::PyProperty::new(
        fget, fset, fdel, doc,
    ))))
}

/// `staticmethod(f)` — non-data descriptor that returns the wrapped
/// callable unchanged on access.
pub fn construct_staticmethod(args: &[Object]) -> Result<Object, RuntimeError> {
    let inner = args.first().cloned().unwrap_or(Object::None);
    Ok(Object::StaticMethod(Rc::new(inner)))
}

/// `classmethod(f)` — non-data descriptor that binds the wrapped
/// callable to the *class* (not the instance) on access.
pub fn construct_classmethod(args: &[Object]) -> Result<Object, RuntimeError> {
    let inner = args.first().cloned().unwrap_or(Object::None);
    Ok(Object::ClassMethod(Rc::new(inner)))
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
        Some(Object::StaticMethod(inner)) => Ok((**inner).clone()),
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
        Some(Object::ClassMethod(i)) => (**i).clone(),
        _ => return Err(type_error("classmethod.__get__() missing self")),
    };
    let owner = match args.get(2) {
        Some(o) if !matches!(o, Object::None) => o.clone(),
        _ => match args.get(1) {
            Some(o) if !matches!(o, Object::None) => Object::Type(class_of(o)),
            _ => {
                return Err(type_error(
                    "classmethod.__get__(None, None) is not valid",
                ))
            }
        },
    };
    Ok(Object::BoundMethod(Rc::new(crate::object::BoundMethod {
        receiver: owner,
        function: inner,
    })))
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
        Some(obj) if !matches!(obj, Object::None) => {
            Ok(Object::BoundMethod(Rc::new(crate::object::BoundMethod {
                receiver: obj.clone(),
                function: func,
            })))
        }
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

fn property_with(
    args: &[Object],
    which: crate::object::PropertyAttr,
) -> Result<Object, RuntimeError> {
    let prop = match args.first() {
        Some(Object::Property(p)) => p.clone(),
        _ => return Err(type_error("expected property as first argument")),
    };
    let fn_ = args.get(1).cloned().unwrap_or(Object::None);
    Ok(Object::Property(Rc::new(prop.with(which, fn_))))
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
    match args.first() {
        Some(Object::Property(p)) => Ok(p.clone()),
        _ => Err(type_error(format!(
            "descriptor '{op}' requires a 'property' object"
        ))),
    }
}

/// `property.__get__(self, obj, objtype=None)` — CPython's
/// `property_descr_get`: class access (obj is None) returns the property
/// itself; instance access invokes `fget`.
fn property_dunder_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = property_self(args, "__get__")?;
    match args.get(1) {
        Some(obj) if !matches!(obj, Object::None) => {
            if matches!(p.fget, Object::None) {
                return Err(crate::error::attribute_error("unreadable attribute"));
            }
            reentrant_call(&p.fget, &[obj.clone()])
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
    if matches!(p.fset, Object::None) {
        return Err(crate::error::attribute_error(
            "property has no setter".to_owned(),
        ));
    }
    reentrant_call(&p.fset, &[obj, value])?;
    Ok(Object::None)
}

/// `property.__delete__(self, obj)` — CPython's deleter slot.
fn property_dunder_delete(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = property_self(args, "__delete__")?;
    let obj = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("__delete__() takes exactly 2 arguments"))?;
    if matches!(p.fdel, Object::None) {
        return Err(crate::error::attribute_error(
            "property has no deleter".to_owned(),
        ));
    }
    reentrant_call(&p.fdel, &[obj])?;
    Ok(Object::None)
}

fn b_getattr(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("getattr() requires at least 2 arguments"));
    }
    let name = match crate::attr_name_of(&args[1]) {
        Some(n) => n,
        None => return Err(type_error("attribute name must be string")),
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
        None => return Err(type_error("attribute name must be string")),
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
        None => return Err(type_error("attribute name must be string")),
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
        None => return Err(type_error("attribute name must be string")),
    };
    Ok(Object::Bool(attr_get(&args[0], &name).is_some()))
}

fn b_vars(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
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

fn b_callable(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "callable")?;
    let intrinsic = matches!(
        v,
        Object::Function(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Type(_)
            | Object::Generator(_)
            // Since Python 3.10 (bpo-43682) `staticmethod` objects are
            // themselves callable, forwarding to the wrapped function.
            | Object::StaticMethod(_)
    );
    if intrinsic {
        return Ok(Object::Bool(true));
    }
    // Instances are callable when their class exposes `__call__`.
    if let Object::Instance(inst) = v {
        return Ok(Object::Bool(inst.cls().lookup("__call__").is_some()));
    }
    Ok(Object::Bool(false))
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
        Object::Function(_) => Object::BoundMethod(Rc::new(crate::object::BoundMethod {
            receiver: receiver.clone(),
            function: value.clone(),
        })),
        Object::StaticMethod(inner) => (**inner).clone(),
        Object::ClassMethod(inner) => {
            let cls = match receiver {
                Object::Instance(inst) => Object::Type(inst.cls()),
                Object::Type(_) => receiver.clone(),
                _ => receiver.clone(),
            };
            Object::BoundMethod(Rc::new(crate::object::BoundMethod {
                receiver: cls,
                function: (**inner).clone(),
            }))
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
                    Object::ClassMethod(inner) => {
                        Object::BoundMethod(Rc::new(crate::object::BoundMethod {
                            receiver: Object::Type(t.clone()),
                            function: (*inner).clone(),
                        }))
                    }
                    Object::StaticMethod(inner) => (*inner).clone(),
                    other => other,
                });
            }
            // Mirror the synthetic dunders served by `Vm::load_attr_type`.
            // We can't reach the VM from here, but these are pure data
            // reads off the TypeObject and safe to inline.
            match name {
                "__name__" | "__qualname__" => Some(Object::from_str(&t.name)),
                "__bases__" => Some(Object::new_tuple(
                    t.bases.iter().map(|b| Object::Type(b.clone())).collect(),
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
                .attrs
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
                "__dict__" => Some(Object::Dict(f.attrs.clone())),
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
                        let mut d = crate::object::DictData::new();
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
            "__module__" => Some(Object::from_static("builtins")),
            "__doc__" => Some(Object::None),
            "__self__" => Some(Object::None),
            _ => None,
        },
        Object::BoundMethod(bm) => match name {
            "__func__" => Some(bm.function.clone()),
            "__self__" => Some(bm.receiver.clone()),
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
                return Some(Object::BoundMethod(Rc::new(crate::object::BoundMethod {
                    receiver: obj.clone(),
                    function: builtin,
                })));
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
        "co_qualname" | "__qualname__" => Some(Object::from_str(&c.name)),
        "co_filename" => Some(Object::from_str(&c.filename)),
        "co_argcount" => Some(Object::Int(i64::from(c.arg_count))),
        "co_posonlyargcount" => Some(Object::Int(i64::from(c.posonly_count))),
        "co_kwonlyargcount" => Some(Object::Int(i64::from(c.kwonly_count))),
        "co_nlocals" => Some(Object::Int(c.varnames.len() as i64)),
        "co_stacksize" => Some(Object::Int(i64::from(c.to_cpython().stacksize))),
        "co_flags" => Some(Object::Int(i64::from(code_flags(c)))),
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
        // compiled from one line reports 1, not 0).
        "co_firstlineno" => Some(Object::Int(i64::from(
            c.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1),
        ))),
        "co_consts" => Some(Object::new_tuple(
            c.constants
                .iter()
                .cloned()
                .map(crate::constant_to_object_public)
                .collect(),
        )),
        // CPython-3.13 wire view (RFC 0033). Computed on demand.
        "co_code" => Some(Object::Bytes(Rc::from(c.to_cpython().co_code))),
        "co_linetable" => Some(Object::Bytes(Rc::from(c.to_cpython().co_linetable))),
        "co_exceptiontable" => Some(Object::Bytes(Rc::from(c.to_cpython().co_exceptiontable))),
        "co_localsplusnames" => Some(Object::new_tuple(
            c.to_cpython()
                .localsplusnames
                .iter()
                .map(Object::from_str)
                .collect(),
        )),
        "co_localspluskinds" => Some(Object::Bytes(Rc::from(c.to_cpython().localspluskinds))),
        "co_lines" => Some(code_method(c, "co_lines", code_co_lines)),
        "co_positions" => Some(code_method(c, "co_positions", code_co_positions)),
        "_varname_from_oparg" => Some(code_method(
            c,
            "_varname_from_oparg",
            code_varname_from_oparg,
        )),
        "replace" => Some(code_method_kw(c, "replace", code_replace)),
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
    Object::BoundMethod(Rc::new(crate::object::BoundMethod {
        receiver: Object::Code(c.clone()),
        function: Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    }))
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

    for (k, v) in kwargs {
        match k.as_str() {
            "co_name" => nc.name = want_str(v, "co_name")?,
            "co_filename" => nc.filename = want_str(v, "co_filename")?,
            "co_argcount" => nc.arg_count = want_u32(v, "co_argcount")?,
            "co_posonlyargcount" => nc.posonly_count = want_u32(v, "co_posonlyargcount")?,
            "co_kwonlyargcount" => nc.kwonly_count = want_u32(v, "co_kwonlyargcount")?,
            "co_varnames" => nc.varnames = want_str_seq(v, "co_varnames")?,
            "co_names" => nc.names = want_str_seq(v, "co_names")?,
            "co_freevars" => nc.freevars = want_str_seq(v, "co_freevars")?,
            "co_cellvars" => nc.cellvars = want_str_seq(v, "co_cellvars")?,
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
            }
            // Recognised CPython fields WeavePy derives on demand rather
            // than storing independently. Accepted (carried through) so
            // `replace()` callers don't break, but not independently set.
            "co_qualname" | "co_flags" | "co_stacksize" | "co_code" | "co_consts"
            | "co_exceptiontable" | "co_nlocals" | "co_lnotab" => {}
            other => {
                return Err(type_error(format!(
                    "replace() got an unexpected keyword argument '{other}'"
                )))
            }
        }
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
    Object::BoundMethod(Rc::new(crate::object::BoundMethod {
        receiver: Object::Code(c.clone()),
        function: Object::Builtin(Rc::new(method(name, body))),
    }))
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
    let cp = c.to_cpython();
    let debug_ranges = crate::vm_singletons::debug_ranges();
    let col = |v: Option<u32>| {
        v.filter(|_| debug_ranges)
            .map_or(Object::None, |x| Object::Int(i64::from(x)))
    };
    let items = cp
        .positions
        .iter()
        .map(|p| {
            Object::new_tuple(vec![
                Object::Int(i64::from(p.lineno)),
                Object::Int(i64::from(p.end_lineno)),
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

/// `code.co_lines()` — `(start, end, lineno)` byte ranges (PEP 626),
/// merging consecutive code units that share a line.
fn code_co_lines(args: &[Object]) -> Result<Object, RuntimeError> {
    let c = code_self(args)?;
    let cp = c.to_cpython();
    let n = cp.positions.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let line = cp.positions[i].lineno;
        let start = i;
        while i < n && cp.positions[i].lineno == line {
            i += 1;
        }
        out.push(Object::new_tuple(vec![
            Object::Int((start * 2) as i64),
            Object::Int((i * 2) as i64),
            Object::Int(i64::from(line)),
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
    c.varnames
        .iter()
        .chain(c.cellvars.iter())
        .chain(c.freevars.iter())
        .nth(idx)
        .map(Object::from_str)
        .ok_or_else(|| type_error("_varname_from_oparg(): index out of range".to_owned()))
}

/// Return the docstring extracted from a code object, if its first
/// constant is a string literal — CPython's `__doc__` convention.
/// The compiler keeps the leading bare string expression as
/// ``constants[0]``; functions / modules / classes pick it up at
/// runtime via this helper.
thread_local! {
    /// Docstring objects keyed by the constant's string-data address, so
    /// repeated `f.__doc__` reads return the *same* `str` object (CPython
    /// stores the docstring once on the function; `update_wrapper` tests
    /// `assertIs(wrapper.__doc__, wrapped.__doc__)`).
    static DOCSTRING_CACHE: std::cell::RefCell<std::collections::HashMap<usize, Object>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

pub(crate) fn code_docstring(c: &weavepy_compiler::CodeObject) -> Option<Object> {
    match c.constants.first() {
        Some(weavepy_compiler::Constant::Str(s)) => Some(DOCSTRING_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .entry(s.as_ptr() as usize)
                .or_insert_with(|| Object::from_str(s.as_str()))
                .clone()
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
    const CO_GENERATOR: u32 = 0x0020;
    const CO_NOFREE: u32 = 0x0040;
    const CO_COROUTINE: u32 = 0x0080;
    const CO_ITERABLE_COROUTINE: u32 = 0x0100;
    const CO_ASYNC_GENERATOR: u32 = 0x0200;
    let mut f = CO_OPTIMIZED | CO_NEWLOCALS;
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
    if c.freevars.is_empty() && c.cellvars.is_empty() {
        f |= CO_NOFREE;
    }
    f
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
                f.attrs
                    .borrow_mut()
                    .insert(crate::object::DictKey(Object::from_str(name)), value);
            }
            Ok(())
        }
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
                f.attrs
                    .borrow_mut()
                    .shift_remove(&crate::object::DictKey(Object::from_str(name)));
            }
            Ok(())
        }
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
                return Err(value_error(if f.is_nan() {
                    "cannot convert float NaN to integer"
                } else {
                    "cannot convert float infinity to integer"
                }));
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
            Object::Long(_) => {
                return Err(value_error("int() base must be >= 2 and <= 36, or 0"))
            }
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
        if !radix_is_pow2 && (raw.len() + 1) / 2 > max_digits as usize {
            return Err(value_error(format!(
                "Exceeds the limit ({max_digits} digits) for integer string conversion; \
                 use sys.set_int_max_str_digits() to increase the limit"
            )));
        }
    }

    let invalid =
        || value_error(format!("invalid literal for int() with base {base}: {}", original.repr()));

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
    if base == 0
        && radix == 10
        && cleaned.starts_with('0')
        && cleaned.bytes().any(|c| c != b'0')
    {
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
    if !v.is_int_like() {
        return Err(type_error(format!(
            "as_integer_ratio: '{}' object is not an integer",
            v.type_name()
        )));
    }
    Ok(Object::new_tuple(vec![v.clone(), Object::Int(1)]))
}

fn int_to_bytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let n_obj = args
        .first()
        .ok_or_else(|| type_error("to_bytes() requires self"))?;
    let n = n_obj
        .as_bigint()
        .ok_or_else(|| type_error("to_bytes(): self is not an integer"))?;
    let length = match args.get(1) {
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
    let byteorder = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        None => "big".to_owned(),
        _ => return Err(type_error("byteorder must be a string")),
    };
    let signed = match args.get(3) {
        Some(o) => o.is_truthy(),
        None => false,
    };
    let bytes = bigint_to_bytes(&n, length, &byteorder, signed)?;
    Ok(Object::new_bytes(bytes))
}

fn int_from_bytes_method(args: &[Object]) -> Result<Object, RuntimeError> {
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
    let data = data_obj
        .as_bytes_view()
        .or_else(|| {
            // Iterables of ints: collect into bytes.
            data_obj.make_iter().ok().map(|mut it| {
                let mut out = Vec::new();
                while let Some(x) = it.next_value() {
                    if let Object::Int(b) = x {
                        if (0..=255).contains(&b) {
                            out.push(b as u8);
                            continue;
                        }
                    }
                    out.clear();
                    return out;
                }
                out
            })
        })
        .ok_or_else(|| type_error("from_bytes() requires bytes-like"))?;
    let byteorder = match args.get(offset + 1) {
        Some(Object::Str(s)) => s.to_string(),
        None => "big".to_owned(),
        _ => return Err(type_error("byteorder must be a string")),
    };
    let signed = match args.get(offset + 2) {
        Some(o) => o.is_truthy(),
        None => false,
    };
    let n = bytes_to_bigint(&data, &byteorder, signed)?;
    Ok(Object::int_from_bigint(n))
}

fn bigint_to_bytes(
    n: &BigInt,
    length: usize,
    byteorder: &str,
    signed: bool,
) -> Result<Vec<u8>, RuntimeError> {
    if !signed && n.is_negative() {
        return Err(value_error("can't convert negative int to unsigned"));
    }
    if length == 0 && !n.is_zero() {
        return Err(value_error("int too big to convert"));
    }
    let bytes = if signed {
        let raw = n.to_signed_bytes_be();
        if raw.len() > length {
            return Err(value_error("int too big to convert"));
        }
        let pad_byte = if n.is_negative() { 0xFF } else { 0x00 };
        let mut out = vec![pad_byte; length - raw.len()];
        out.extend_from_slice(&raw);
        out
    } else {
        let (_, raw) = n.to_bytes_be();
        if raw.len() > length {
            return Err(value_error("int too big to convert"));
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
    Ok(Object::int_from_bigint(crate::object::bigint_from_f64_trunc(
        f,
    )))
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
    let (m_hex, exponent) = if exp_field == 0 {
        // Subnormal
        let mut hex = format!("{:013x}", mantissa);
        // Trim trailing zeroes for compactness (CPython keeps full
        // 13 hex digits for subnormals; we follow suit).
        let _ = &mut hex;
        (format!("0x0.{hex}"), -1022)
    } else {
        let mut hex = format!("{:013x}", mantissa);
        // Trim trailing zeroes in the fractional part.
        while hex.ends_with('0') {
            hex.pop();
        }
        if hex.is_empty() {
            hex.push('0');
        }
        (format!("0x1.{hex}"), exp_field - 1023)
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
    let overflow = || crate::error::overflow_error("hexadecimal value too large to represent as a float");

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
    let coeff_end: usize;
    if i < n && bytes[i] == b'.' {
        i += 1;
        while i < n && hex_from_byte(bytes[i]) >= 0 {
            i += 1;
        }
        coeff_end = i - 1;
    } else {
        coeff_end = i;
    }

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
        exp = exp_text.parse::<i64>().unwrap_or(if bytes[exp_start] == b'-' {
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
        b'0'..=b'9' => (b - b'0') as i32,
        b'a'..=b'f' => (b - b'a' + 10) as i32,
        b'A'..=b'F' => (b - b'A' + 10) as i32,
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
        Some((if negate { f64::NEG_INFINITY } else { f64::INFINITY }, i))
    } else if ci_match(s, i, b"nan") {
        i += 3;
        Some((if negate { -f64::NAN } else { f64::NAN }, i))
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

pub(crate) fn b_int_from_bytes_cls(args: &[Object]) -> Result<Object, RuntimeError> {
    int_from_bytes_method(args)
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
        Object::Str(_) | Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_) => {
            // str / bytes-like: bytes-like buffers are decoded as ASCII-ish
            // text; non-UTF-8 input simply fails to parse (CPython raises the
            // same ValueError).
            let text: Option<String> = match &args[0] {
                Object::Str(s) => Some(s.to_string()),
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
        "nan" | "+nan" => return Some(f64::NAN),
        // Preserve the sign bit so `copysign(1.0, float('-nan'))` is -1.0.
        "-nan" => return Some(-f64::NAN),
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
/// `None`. Each `Nd` block is exactly ten consecutive code points `0..=9`,
/// so the block's zero is found by walking down while still in category
/// `Nd` (bounded to nine steps).
fn unicode_decimal_value(c: char) -> Option<u32> {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    if c.general_category() != GeneralCategory::DecimalNumber {
        return None;
    }
    let cp = c as u32;
    let mut zero = cp;
    while cp - zero < 9 {
        match char::from_u32(zero - 1) {
            Some(p) if p.general_category() == GeneralCategory::DecimalNumber => zero -= 1,
            _ => break,
        }
    }
    Some(cp - zero)
}

/// PEP 515 underscore rule for decimal float literals: every `_` must sit
/// directly between two ASCII digits (so `1_000` is fine but `_1`, `1_`,
/// `1__0`, `1_.0`, `1e_5` are not).
fn valid_float_underscores(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'_'
            && !(i > 0
                && b[i - 1].is_ascii_digit()
                && i + 1 < b.len()
                && b[i + 1].is_ascii_digit())
        {
            return false;
        }
    }
    true
}

fn b_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Bool(false));
    }
    Ok(Object::Bool(args[0].is_truthy()))
}

pub(crate) fn b_complex(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_complex(0.0, 0.0));
    }
    let has_second = args.len() >= 2;
    // CPython's `complex_new` ordering: a string `real` is only valid as the
    // sole argument; a string `imag` is never valid. Both checks precede the
    // numeric coercion (so e.g. `complex({}, '1')` reports the string, not the
    // dict).
    if let Object::Str(s) = &args[0] {
        if has_second {
            return Err(type_error(
                "complex() can't take second arg if first is a string",
            ));
        }
        return parse_complex_string(s).map(|(r, i)| Object::new_complex(r, i));
    }
    if has_second && matches!(&args[1], Object::Str(_)) {
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
            args[0].as_f64().expect("numeric")
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
                b.as_f64().expect("numeric")
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
    let starts = |word: &[u8]| rest.len() >= word.len() && rest[..word.len()].eq_ignore_ascii_case(word);
    let finish = |end: usize| -> Option<(f64, usize)> {
        let slice = std::str::from_utf8(&b[..end]).ok()?;
        slice.parse::<f64>().ok().map(|v| (v, end))
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
    if args.is_empty() {
        return Ok(Object::new_list(Vec::new()));
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_list(out))
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
        return Ok(Object::new_dict());
    }
    // Fast path: another built-in dict copies entry-for-entry. Avoids
    // re-iterating as a sequence of pairs (which would fail, since
    // iter(dict) yields keys, not items).
    if let Object::Dict(src) = &args[0] {
        let mut d = DictData::new();
        for (k, v) in src.borrow().iter() {
            d.insert(k.clone(), v.clone());
        }
        return Ok(Object::Dict(Rc::new(RefCell::new(d))));
    }
    // Mapping path for user-defined classes (`__keys__` style) is
    // handled by the VM before dispatching here — see
    // `Vm::do_dict_call`. Anything left over is an iterable of pairs.
    let mut it = args[0].make_iter()?;
    let mut d = DictData::new();
    while let Some(pair) = it.next_value() {
        match pair {
            Object::Tuple(items) if items.len() == 2 => {
                d.insert(DictKey(items[0].clone()), items[1].clone());
            }
            Object::List(items) => {
                let items = items.borrow();
                if items.len() == 2 {
                    d.insert(DictKey(items[0].clone()), items[1].clone());
                } else {
                    return Err(value_error(
                        "dictionary update sequence element is not a 2-element sequence",
                    ));
                }
            }
            _ => {
                return Err(value_error(
                    "dictionary update sequence element is not a 2-tuple",
                ))
            }
        }
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

fn b_type(args: &[Object]) -> Result<Object, RuntimeError> {
    // `type(name, bases, ns)` is intercepted by the VM call site
    // (see `Vm::dynamic_type_call`); only the 1-arg form reaches us.
    let arg = one(args, "type")?;
    Ok(Object::Type(class_of(arg)))
}

fn b_set(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_set());
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_set_from(out))
}

fn b_frozenset(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_frozenset_from(Vec::new()));
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_frozenset_from(out))
}

/// One item of a `bytes(iterable)` source: an integer in
/// `range(0, 256)` via the `__index__` protocol.
fn byte_item_value(o: &Object) -> Result<u8, RuntimeError> {
    let native = o.native_value();
    match native.as_ref().unwrap_or(o) {
        Object::Bool(b) => Ok(u8::from(*b)),
        Object::Int(i) if (0..=255).contains(i) => Ok(*i as u8),
        Object::Int(_) | Object::Long(_) => {
            Err(value_error("bytes must be in range(0, 256)"))
        }
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
        Object::MemoryView(mv) => Ok(mv.to_bytes()),
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
                        let r =
                            interp.call_object_with_globals(&method, &[], &[], &globals)?;
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
            let indexable = inst.native.as_ref().map(|n| n.as_i64().is_some()).unwrap_or(false)
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
            if let Some(native) = &inst.native {
                if matches!(
                    native,
                    Object::Bytes(_) | Object::ByteArray(_) | Object::MemoryView(_)
                ) {
                    return bytes_from_source_obj(&native.clone(), type_name);
                }
            }
            // Iterable (including legacy `__getitem__` sequences) via
            // interpreter reentry; `__iter__` exceptions propagate.
            let iterable = crate::instance_method(src, "__iter__").is_some()
                || crate::instance_method(src, "__getitem__").is_some()
                || inst.native.is_some();
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
            if other.make_iter().is_err() && !matches!(other, Object::Generator(_)) {
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
fn bytes_from_iterable_reentrant(
    src: &Object,
    type_name: &str,
) -> Result<Vec<u8>, RuntimeError> {
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
        Object::Instance(inst) => match &inst.native {
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
    Ok(Object::new_bytes(bytes_construct(args, kwargs, "bytes")?))
}

fn b_bytes(args: &[Object]) -> Result<Object, RuntimeError> {
    b_bytes_kw(args, &[])
}

fn b_bytearray_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    Ok(Object::new_bytearray(bytes_construct(
        args, kwargs, "bytearray",
    )?))
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
    // Reuse the positional path. We fold known kwargs into positional
    // slots and accept (but ignore) the rest.
    let mut combined: Vec<Object> = args.to_vec();
    let mut mode = combined.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "mode" => mode = Some(v.clone()),
            "buffering" | "encoding" | "errors" | "newline" | "closefd" | "opener" => {
                // Accept but don't fail: encoding is implicitly utf-8.
            }
            other => {
                return Err(type_error(format!(
                    "open() got an unexpected keyword argument '{other}'"
                )));
            }
        }
    }
    if let Some(m) = mode {
        if combined.len() < 2 {
            combined.push(m);
        } else {
            combined[1] = m;
        }
    }
    b_open(&combined)
}

fn b_open(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    use std::fs::OpenOptions;
    if args.is_empty() {
        return Err(type_error("open() missing required argument: 'file'"));
    }
    let path = match &args[0] {
        Object::Str(s) => s.to_string(),
        _ => return Err(type_error("open() argument 'file' must be str".to_owned())),
    };
    let mode = match args.get(1) {
        Some(Object::Str(m)) => m.to_string(),
        Some(_) => return Err(type_error("open() mode must be str")),
        None => "r".to_owned(),
    };
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
    let f = opts
        .open(&path)
        .map_err(|e| crate::error::os_error(format!("{path}: {e}")))?;
    Ok(Object::File(Rc::new(PyFile::new(
        path,
        mode,
        FileBackend::Disk(f),
    ))))
}

pub(crate) fn b_abs(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "abs")? {
        Object::Int(i) => match i.checked_abs() {
            Some(v) => Ok(Object::Int(v)),
            // i64::MIN.abs() overflows; promote.
            None => Ok(Object::int_from_bigint(num_bigint::BigInt::from(*i).abs())),
        },
        Object::Long(b) => Ok(Object::int_from_bigint(b.abs())),
        Object::Float(f) => Ok(Object::Float(f.abs())),
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
    let iterable = one(args, "reversed")?;
    // Materialize the source in *forward* order; the Reversed iterator
    // walks it back-to-front. (CPython's `reversed` uses `__reversed__`
    // or `__len__`+`__getitem__`; a forward snapshot reproduces the same
    // sequence for the finite iterables WeavePy handles here.)
    let mut it = iterable.make_iter()?;
    let mut buf = Vec::new();
    while let Some(v) = it.next_value() {
        buf.push(v);
    }
    let index = buf.len() as i64 - 1;
    Ok(Object::Iter(Rc::new(RefCell::new(PyIterator::Reversed {
        items: Rc::new(RefCell::new(buf)),
        index,
    }))))
}

fn b_enumerate(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "enumerate")?;
    let start = if args.len() >= 2 {
        match &args[1] {
            Object::Int(i) => *i,
            _ => return Err(type_error("enumerate() start must be an int")),
        }
    } else {
        0
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

fn b_super(args: &[Object]) -> Result<Object, RuntimeError> {
    // `super(C, self)` returns a proxy instance whose class is the
    // synthesized proxy type. Zero-arg form is handled by the VM's
    // call path (it materialises `__class__` and `self` first).
    if args.len() != 2 {
        return Err(type_error(
            "super(): expected 2 arguments (zero-arg form must be called from inside a method)",
        ));
    }
    let class = match &args[0] {
        Object::Type(t) => t.clone(),
        _ => return Err(type_error("super() arg 1 must be a class")),
    };
    let receiver = args[1].clone();
    Ok(make_super(class, receiver))
}

/// Construct a super proxy. Exposed publicly so the VM can build
/// zero-arg super objects.
pub fn make_super(class: Rc<crate::types::TypeObject>, receiver: Object) -> Object {
    use crate::types::TypeObject;
    // CPython's `super(C, obj_or_type)` (see `super_init_impl`) chooses
    // which MRO to walk from the *second* argument:
    //   * `obj` is an instance        → walk `type(obj)`'s MRO.
    //   * `obj` is a type & subclass  → "bound-to-subclass" form, walk
    //     of `C`                        `obj`'s own MRO (classmethods and
    //                                   the implicit `super()` inside
    //                                   `__init_subclass__` / `__new__`).
    //   * `obj` is a type but NOT a   → metaclass-method form (`obj` is an
    //     subclass of `C`               *instance* of the metaclass `C`),
    //                                   walk `type(obj)`'s MRO.
    // Collapsing the two type cases into one (always `obj`'s MRO, or
    // always `C`'s MRO) breaks either diamond `__init_subclass__` or
    // `super().__init__()` inside a metaclass, respectively.
    let receiver_class = match &receiver {
        Object::Instance(inst) => inst.cls(),
        Object::Type(t) if t.is_subclass_of(&class) => t.clone(),
        Object::Type(t) => t.metaclass_or_type(),
        _ => class.clone(),
    };
    let mro = receiver_class.mro.borrow();
    let start = mro
        .iter()
        .position(|t| Rc::ptr_eq(t, &class))
        .map_or(mro.len(), |i| i + 1);
    let after: Vec<_> = mro[start..].to_vec();
    drop(mro);
    let proxy = Rc::new(TypeObject {
        name: format!("super<{}>", class.name),
        bases: after.clone(),
        mro: RefCell::new(after),
        dict: Rc::new(RefCell::new(DictData::new())),
        flags: crate::types::TypeFlags::default(),
        metaclass: RefCell::new(None),
        slot_names: RefCell::new(Vec::new()),
        forbids_dict: false,
        subclasses: RefCell::new(Vec::new()),
        getattribute_kind: crate::sync::Cell::new(0),
    });
    let inst = crate::types::PyInstance {
        class: RefCell::new(proxy),
        dict: Rc::new(RefCell::new({
            let mut d = DictData::new();
            d.insert(DictKey(Object::from_static("__self__")), receiver);
            // CPython's `su->obj_type` — the class whose MRO is walked,
            // passed as `owner` to descriptor `__get__`s. Also used to
            // detect the class-bound form (`su->obj == starttype`),
            // where descriptors get a NULL instance (so plain functions
            // come back *unbound*: `super().__new__(cls, v)` must not
            // prepend a second `cls`).
            d.insert(
                DictKey(Object::from_static("__obj_type__")),
                Object::Type(receiver_class.clone()),
            );
            d
        })),
        native: None,
        inline_values: crate::sync::Cell::new(true),
        slots: crate::sync::RefCell::new(None),
    };
    Object::Instance(Rc::new(inst))
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
        Object::Type(t) => Ok(cls.is_subclass_of(t)),
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
                    if cls.is_subclass_of(t) {
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
        Object::Tuple(_) => bt.tuple_.clone(),
        Object::List(_) => bt.list_.clone(),
        Object::Dict(_) => bt.dict_.clone(),
        Object::Range(_) => bt.range_.clone(),
        Object::Slice(_) => bt.slice_.clone(),
        Object::MemoryView(_) => bt.memoryview_.clone(),
        Object::MappingProxy(_) => bt.mappingproxy_.clone(),
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
                bt.generic_alias_.clone()
            } else {
                bt.simple_namespace_.clone()
            }
        }
        Object::Type(t) => t.metaclass_or_type(),
        Object::Function(_) => bt.function_.clone(),
        // Rust-implemented callables are `builtin_function_or_method`,
        // distinct from `function`, exactly as in CPython (`type(len)`).
        Object::Builtin(_) => bt.builtin_function_.clone(),
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
                if n.starts_with("__") && n.ends_with("__") {
                    bt.method_wrapper_.clone()
                } else {
                    bt.builtin_function_.clone()
                }
            }
            _ => bt.method_.clone(),
        },
        Object::Property(_) => bt.property_.clone(),
        Object::StaticMethod(_) => bt.staticmethod_.clone(),
        Object::ClassMethod(_) => bt.classmethod_.clone(),
        Object::Bytes(_) => bt.bytes_.clone(),
        Object::ByteArray(_) => bt.bytearray_.clone(),
        Object::Set(_) => bt.set_.clone(),
        Object::FrozenSet(_) => bt.frozenset_.clone(),
        Object::Iter(_) => bt.iterator_.clone(),
        // Native itertools adapters share the generic iterator type for
        // now; `type(x).__name__` is "iterator" rather than CPython's
        // "islice" until they get dedicated TypeObjects.
        Object::LazyIter(_) => bt.iterator_.clone(),
        Object::Generator(_) => bt.generator_.clone(),
        Object::Coroutine(_) => bt.coroutine_.clone(),
        Object::AsyncGenerator(_) => bt.async_generator_.clone(),
        // The transient `asend`/`athrow`/`aclose` awaitables have no
        // dedicated singleton type; treat them as plain objects for
        // `type()` (their faithful CPython name is still surfaced by
        // `repr`/error messages via `Object::type_name`).
        Object::AsyncGenAwait(_) => bt.object_.clone(),
        Object::Module(_) => bt.module_.clone(),
        Object::SlotDescriptor(_) => bt.member_descriptor_.clone(),
        Object::Code(_) => bt.code_.clone(),
        Object::Cell(_) | Object::File(_) => bt.object_.clone(),
        Object::Frame(_) => bt.frame_.clone(),
        Object::Traceback(_) => bt.traceback_.clone(),
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
fn object_identity(obj: &Object) -> i64 {
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
        Object::DictView(v) => Rc::as_ptr(v) as usize as i64,
        Object::SimpleNamespace(n) => Rc::as_ptr(n) as usize as i64,
        Object::Code(c) => Rc::as_ptr(c) as usize as i64,
        Object::Cell(c) => Rc::as_ptr(c) as usize as i64,
        Object::Iter(i) => Rc::as_ptr(i) as usize as i64,
        Object::LazyIter(l) => Rc::as_ptr(l) as usize as i64,
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
        Object::Slice(_) => "slice",
        Object::Tuple(items) => {
            for it in items.iter() {
                ensure_hashable(it)?;
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    Err(type_error(format!("unhashable type: '{name}'")))
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

/// `dir(obj)` — return a sorted list of names available on *obj*.
/// Mirrors CPython's "best effort" introspection: walk the class
/// MRO, the instance dict, the module dict, or — for built-ins —
/// fall back to a small list of dunder names. We deliberately keep
/// this loose because runtime helpers (typing, dataclasses, abc)
/// only need it to enumerate user attributes.
pub(crate) fn b_dir(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();
    let obj = one(args, "dir")?;
    match obj {
        Object::Instance(inst) => {
            for k in inst.dict.borrow().keys() {
                if let Object::Str(s) = &k.0 {
                    names.insert(s.to_string());
                }
            }
            for t in inst.cls().mro.borrow().iter() {
                for k in t.dict.borrow().keys() {
                    if let Object::Str(s) = &k.0 {
                        names.insert(s.to_string());
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
        }
        Object::Module(m) => {
            for k in m.dict.borrow().keys() {
                if let Object::Str(s) = &k.0 {
                    names.insert(s.to_string());
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
            // The generator family's methods and introspection attrs are
            // synthesized in `load_attr` rather than stored in type
            // dicts; surface the same names CPython's type dicts hold.
            let extra: &[&str] = match other {
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
                _ => &[],
            };
            for n in extra {
                names.insert((*n).to_string());
            }
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
        // The `__index__` protocol (CPython `PyNumber_Index`).
        other => b_hex(&[Object::Int(coerce_index_i64(other)?)]),
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
        other => b_oct(&[Object::Int(coerce_index_i64(other)?)]),
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
        // The `__index__` protocol (CPython `PyNumber_Index`).
        other => b_bin(&[Object::Int(coerce_index_i64(other)?)]),
    }
}

fn b_chr(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "chr")? {
        Object::Int(i) => {
            let ch = char::from_u32(*i as u32)
                .ok_or_else(|| value_error("chr() arg not in range(0x110000)"))?;
            Ok(Object::from_str(ch.to_string()))
        }
        _ => Err(type_error("chr() expected int")),
    }
}

fn b_ord(args: &[Object]) -> Result<Object, RuntimeError> {
    let arg = one(args, "ord")?;
    let native = arg.native_value();
    match native.as_ref().unwrap_or(arg) {
        Object::Str(s) => {
            let mut chars = s.chars();
            let c = chars
                .next()
                .ok_or_else(|| type_error("ord() expected a character, but string of length 0 found"))?;
            if chars.next().is_some() {
                return Err(type_error(format!(
                    "ord() expected a character, but string of length {} found",
                    s.chars().count()
                )));
            }
            Ok(Object::Int(i64::from(u32::from(c))))
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

/// `pow(base, exp[, mod])` — modular exponentiation when `mod` is
/// given, otherwise `base ** exp`. Mirrors CPython's three-arg
/// `pow` including the negative-exponent + mod case (the modular
/// inverse).
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
        Ok(Object::new_complex(magnitude * theta.cos(), magnitude * theta.sin()))
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
            None => {
                return Err(value_error(
                    "base is not invertible for the given modulus",
                ))
            }
        }
    }
    let mut result: BigInt = BigInt::one();
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

/// `__weavepy_typevar__(name)` — internal builtin that produces a
/// lightweight `TypeVar`-shaped placeholder. Used as the implicit
/// binding for PEP 695 type parameters in `type X[T] = ...`,
/// `def f[T](...)`, and `class C[T]:`. Behaves enough like
/// `typing.TypeVar` that consumers can subscript / index / repr it
/// without importing `typing`.
fn b_typevar(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => other.to_str(),
        None => "T".to_owned(),
    };
    let mut d = crate::object::DictData::new();
    d.insert(
        crate::object::DictKey(Object::from_static("__name__")),
        Object::from_str(&name),
    );
    d.insert(
        crate::object::DictKey(Object::from_static("__weavepy_typevar__")),
        Object::Bool(true),
    );
    Ok(Object::SimpleNamespace(Rc::new(crate::sync::RefCell::new(
        d,
    ))))
}

/// `memoryview(obj)` — returns a `MemoryView` over a bytes-like
/// object. We accept `bytes`, `bytearray`, and existing
/// `MemoryView` (which we shallow-copy, matching CPython).
pub fn b_memoryview(args: &[Object]) -> Result<Object, RuntimeError> {
    let arg = one(args, "memoryview")?;
    let mv = match arg {
        Object::Bytes(b) => crate::object::PyMemoryView::from_bytes(b.clone()),
        Object::ByteArray(b) => crate::object::PyMemoryView::from_bytearray(b.clone()),
        Object::MemoryView(mv) => {
            // Shallow clone — same backing buffer, same window.
            crate::object::PyMemoryView {
                buffer: match &mv.buffer {
                    crate::object::MemoryViewBuffer::Bytes(b) => {
                        crate::object::MemoryViewBuffer::Bytes(b.clone())
                    }
                    crate::object::MemoryViewBuffer::ByteArray(b) => {
                        crate::object::MemoryViewBuffer::ByteArray(b.clone())
                    }
                },
                start: crate::sync::Cell::new(mv.start.get()),
                len: crate::sync::Cell::new(mv.len.get()),
                readonly: crate::sync::Cell::new(mv.readonly.get()),
                released: crate::sync::Cell::new(mv.released.get()),
                format: crate::sync::RefCell::new(mv.format.borrow().clone()),
                itemsize: crate::sync::Cell::new(mv.itemsize.get()),
            }
        }
        other => {
            return Err(type_error(format!(
                "memoryview: a bytes-like object is required, not '{}'",
                other.type_name()
            )));
        }
    };
    Ok(Object::MemoryView(Rc::new(mv)))
}

fn b_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let it = one(args, "next")?;
    let default = args.get(1).cloned();
    if let Object::Iter(it) = it {
        match it.borrow_mut().next_value() {
            Some(v) => Ok(v),
            None => default.ok_or_else(stop_iteration),
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
        defaults: f.defaults.clone(),
        kw_defaults: f.kw_defaults.clone(),
        closure: f.closure.clone(),
        // Shared, not copied: `func.__dict__` mutations stay visible on
        // both, matching CPython where the function object is the same.
        attrs: f.attrs.clone(),
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
    let q = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::FloorDiv)?;
    let r = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::Mod)?;
    Ok(Object::new_tuple(vec![q, r]))
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
            Some(b.to_i64().unwrap_or(if b.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            }))
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
            // to `n` decimal places.
            Some(n) => double_round(*f, n).map(Object::Float),
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
    if r >= -(9.223_372_036_854_776e18) && r < 9.223_372_036_854_776e18 {
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

fn str_self(args: &[Object]) -> Result<&str, RuntimeError> {
    match args.first() {
        Some(Object::Str(s)) => Ok(s),
        _ => Err(type_error("expected str method receiver")),
    }
}

fn str_upper(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.to_uppercase()))
}

fn str_lower(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.to_lowercase()))
}

fn str_strip(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.trim().to_owned()))
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
/// are dropped, matching CPython.
fn str_split_whitespace(s: &str, maxsplit: i64) -> Vec<Object> {
    if maxsplit < 0 {
        return s.split_whitespace().map(Object::from_str).collect();
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut splits = 0;
    while i < n {
        while i < n && chars[i].1.is_whitespace() {
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
        while i < n && !chars[i].1.is_whitespace() {
            i += 1;
        }
        let end = if i < n { chars[i].0 } else { s.len() };
        out.push(Object::from_str(s[start..end].to_string()));
        splits += 1;
    }
    out
}

fn str_split(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sep = arg_or_kw(args, 1, kwargs, "sep");
    let maxsplit = split_maxsplit(arg_or_kw(args, 2, kwargs, "maxsplit"))?;
    let out: Vec<Object> = match sep {
        None | Some(Object::None) => str_split_whitespace(s, maxsplit),
        Some(Object::Str(sep)) => {
            if sep.is_empty() {
                return Err(value_error("empty separator"));
            }
            if maxsplit < 0 {
                s.split(&**sep).map(Object::from_str).collect()
            } else {
                s.splitn((maxsplit as usize).saturating_add(1), &**sep)
                    .map(Object::from_str)
                    .collect()
            }
        }
        Some(_) => return Err(type_error("must be str or None, not other")),
    };
    Ok(Object::new_list(out))
}

fn str_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let sep = str_self(args)?.to_owned();
    if args.len() != 2 {
        return Err(type_error("join() expected 1 argument"));
    }
    let mut it = args[1].make_iter()?;
    let mut parts = Vec::new();
    while let Some(v) = it.next_value() {
        match v {
            Object::Str(s) => parts.push(s.to_string()),
            other => {
                return Err(type_error(format!(
                    "sequence item: expected str instance, {} found",
                    other.type_name()
                )))
            }
        }
    }
    Ok(Object::from_str(parts.join(&sep)))
}

fn str_startswith(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    // PEP 257: ``startswith`` accepts either a string *or* a tuple of strings.
    let target = match args.get(1) {
        Some(obj) => obj,
        None => return Err(type_error("startswith() takes at least 1 argument")),
    };
    let slice = str_apply_start_end(s, args.get(2), args.get(3))?;
    Ok(Object::Bool(str_match_prefix_suffix(slice, target, true)?))
}

fn str_endswith(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let target = match args.get(1) {
        Some(obj) => obj,
        None => return Err(type_error("endswith() takes at least 1 argument")),
    };
    let slice = str_apply_start_end(s, args.get(2), args.get(3))?;
    Ok(Object::Bool(str_match_prefix_suffix(slice, target, false)?))
}

fn str_apply_start_end<'a>(
    s: &'a str,
    start: Option<&Object>,
    end: Option<&Object>,
) -> Result<&'a str, RuntimeError> {
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
    let start_idx = start_idx.clamp(0, n) as usize;
    let end_idx = end_idx.clamp(0, n) as usize;
    if start_idx > end_idx {
        return Ok("");
    }
    let start_byte = chars.get(start_idx).map(|(i, _)| *i).unwrap_or(s.len());
    let end_byte = chars.get(end_idx).map(|(i, _)| *i).unwrap_or(s.len());
    Ok(&s[start_byte..end_byte])
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
        Object::Str(s) => Ok(test(s)),
        Object::Tuple(parts) => {
            for item in parts.iter() {
                match item {
                    Object::Str(s) => {
                        if test(s) {
                            return Ok(true);
                        }
                    }
                    _ => {
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

fn str_replace(args: &[Object]) -> Result<Object, RuntimeError> {
    str_replace_kw(args, &[])
}

fn str_replace_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let from = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("replace() expected str")),
    };
    let to = match args.get(2) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("replace() expected str")),
    };
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
        return Ok(Object::from_str(s.to_string()));
    }
    let out = if count < 0 {
        s.replace(&**from, to)
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
        s.replacen(&**from, to, count as usize)
    };
    Ok(Object::from_str(out))
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
    let s = str_self(args)?;
    let sub = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("find() expected str")),
    };
    let total_chars = s.chars().count() as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(-1));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    let hay = &s[start_byte..end_byte];
    match hay.find(&**sub) {
        Some(byte_idx) => {
            let abs_byte = byte_idx + start_byte;
            Ok(Object::Int(byte_offset_to_char(s, abs_byte) as i64))
        }
        None => Ok(Object::Int(-1)),
    }
}

fn char_offset_to_byte(s: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    s.char_indices().nth(n).map(|(b, _)| b).unwrap_or(s.len())
}

fn byte_offset_to_char(s: &str, byte: usize) -> usize {
    s[..byte].chars().count()
}

fn str_title(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out = String::new();
    let mut prev_alpha = false;
    for ch in str_self(args)?.chars() {
        if ch.is_alphabetic() {
            if prev_alpha {
                out.extend(ch.to_lowercase());
            } else {
                out.extend(ch.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(ch);
            prev_alpha = false;
        }
    }
    Ok(Object::from_str(out))
}

fn str_capitalize(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let mut chars = s.chars();
    let out = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    };
    Ok(Object::from_str(out))
}

fn str_swapcase(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out = String::new();
    for ch in str_self(args)?.chars() {
        if ch.is_uppercase() {
            out.extend(ch.to_lowercase());
        } else if ch.is_lowercase() {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    Ok(Object::from_str(out))
}

fn str_lstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let out = match args.get(1) {
        None | Some(Object::None) => s.trim_start().to_owned(),
        Some(Object::Str(chars)) => {
            let set: Vec<char> = chars.chars().collect();
            s.trim_start_matches(|c| set.contains(&c)).to_owned()
        }
        _ => return Err(type_error("lstrip() argument must be str")),
    };
    Ok(Object::from_str(out))
}

fn str_rstrip(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let out = match args.get(1) {
        None | Some(Object::None) => s.trim_end().to_owned(),
        Some(Object::Str(chars)) => {
            let set: Vec<char> = chars.chars().collect();
            s.trim_end_matches(|c| set.contains(&c)).to_owned()
        }
        _ => return Err(type_error("rstrip() argument must be str")),
    };
    Ok(Object::from_str(out))
}

/// `str.rsplit` on runs of whitespace, honouring `maxsplit` from the
/// right. Mirrors CPython: the *last* `maxsplit` whitespace runs split,
/// and the left remainder keeps its internal spacing.
fn str_rsplit_whitespace(s: &str, maxsplit: i64) -> Vec<Object> {
    if maxsplit < 0 {
        return s.split_whitespace().map(Object::from_str).collect();
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len();
    let mut out_rev: Vec<String> = Vec::new();
    let mut i = n;
    let mut splits = 0;
    while i > 0 {
        while i > 0 && chars[i - 1].1.is_whitespace() {
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
        while i > 0 && !chars[i - 1].1.is_whitespace() {
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
    let s = str_self(args)?;
    let sep = arg_or_kw(args, 1, kwargs, "sep");
    let maxsplit = split_maxsplit(arg_or_kw(args, 2, kwargs, "maxsplit"))?;
    let out: Vec<Object> = match sep {
        None | Some(Object::None) => str_rsplit_whitespace(s, maxsplit),
        Some(Object::Str(sep)) => {
            if sep.is_empty() {
                return Err(value_error("empty separator"));
            }
            let mut pieces: Vec<&str> = if maxsplit < 0 {
                s.split(&**sep).collect()
            } else {
                let mut v: Vec<&str> = s.rsplitn((maxsplit as usize).saturating_add(1), &**sep).collect();
                v.reverse();
                v
            };
            pieces.drain(..).map(Object::from_str).collect()
        }
        Some(_) => return Err(type_error("must be str or None, not other")),
    };
    Ok(Object::new_list(out))
}

fn str_splitlines(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let keepends = arg_or_kw(args, 1, kwargs, "keepends")
        .map(Object::is_truthy)
        .unwrap_or(false);
    let mut out: Vec<Object> = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' || bytes[i] == b'\r' {
            let end_no_eol = i;
            let mut end = i + 1;
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                end = i + 2;
            }
            let line = if keepends {
                &s[start..end]
            } else {
                &s[start..end_no_eol]
            };
            out.push(Object::from_str(line.to_owned()));
            start = end;
            i = end;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        out.push(Object::from_str(s[start..].to_owned()));
    }
    Ok(Object::new_list(out))
}

fn str_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sub = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("rfind() expected str")),
    };
    let total_chars = s.chars().count() as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(-1));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    let hay = &s[start_byte..end_byte];
    match hay.rfind(&**sub) {
        Some(byte_idx) => {
            let abs_byte = byte_idx + start_byte;
            Ok(Object::Int(byte_offset_to_char(s, abs_byte) as i64))
        }
        None => Ok(Object::Int(-1)),
    }
}

fn str_index(args: &[Object]) -> Result<Object, RuntimeError> {
    let pos = str_find(args)?;
    match pos {
        Object::Int(-1) => Err(value_error("substring not found")),
        other => Ok(other),
    }
}

fn str_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sub = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("count() expected str")),
    };
    let total_chars = s.chars().count() as i64;
    let Some((start, end)) = str_search_window(args, total_chars) else {
        return Ok(Object::Int(0));
    };
    let start_byte = char_offset_to_byte(s, start as usize);
    let end_byte = char_offset_to_byte(s, end as usize);
    Ok(Object::Int(
        s[start_byte..end_byte].matches(&**sub).count() as i64
    ))
}

fn str_partition(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sep = match args.get(1) {
        Some(Object::Str(p)) => p.to_string(),
        _ => return Err(type_error("partition() expected str")),
    };
    let (head, tail) = match s.find(&sep) {
        Some(i) => (s[..i].to_owned(), s[i + sep.len()..].to_owned()),
        None => {
            return Ok(Object::new_tuple(vec![
                Object::from_str(s.to_owned()),
                Object::from_static(""),
                Object::from_static(""),
            ]))
        }
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(head),
        Object::from_str(sep),
        Object::from_str(tail),
    ]))
}

fn str_rpartition(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sep = match args.get(1) {
        Some(Object::Str(p)) => p.to_string(),
        _ => return Err(type_error("rpartition() expected str")),
    };
    let (head, tail) = match s.rfind(&sep) {
        Some(i) => (s[..i].to_owned(), s[i + sep.len()..].to_owned()),
        None => {
            return Ok(Object::new_tuple(vec![
                Object::from_static(""),
                Object::from_static(""),
                Object::from_str(s.to_owned()),
            ]))
        }
    };
    Ok(Object::new_tuple(vec![
        Object::from_str(head),
        Object::from_str(sep),
        Object::from_str(tail),
    ]))
}

fn str_isdigit(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()),
    ))
}

fn str_isalpha(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(char::is_alphabetic),
    ))
}

fn str_isalnum(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(char::is_alphanumeric),
    ))
}

fn str_isspace(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    Ok(Object::Bool(
        !s.is_empty() && s.chars().all(char::is_whitespace),
    ))
}

fn str_isupper(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let mut has_cased = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            has_cased = true;
            if !c.is_uppercase() {
                return Ok(Object::Bool(false));
            }
        }
    }
    Ok(Object::Bool(has_cased))
}

fn str_islower(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let mut has_cased = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            has_cased = true;
            if !c.is_lowercase() {
                return Ok(Object::Bool(false));
            }
        }
    }
    Ok(Object::Bool(has_cased))
}

fn str_isascii(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(str_self(args)?.is_ascii()))
}

fn str_isidentifier(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let mut chars = s.chars();
    let valid = match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => chars.all(|c| c == '_' || c.is_alphanumeric()),
        _ => false,
    };
    Ok(Object::Bool(valid))
}

fn str_isprintable(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    Ok(Object::Bool(
        s.chars().all(crate::object::char_is_printable),
    ))
}

fn str_zfill(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let width = match args.get(1) {
        // A negative width is a no-op in CPython (`'x'.zfill(-3) == 'x'`);
        // clamp to 0 so `*i as usize` can't wrap to a gigantic pad count.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        _ => return Err(type_error("zfill() expected int")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(Object::from_str(s.to_owned()));
    }
    let pad = width - len;
    let (sign, rest) = if s.starts_with('+') || s.starts_with('-') {
        (&s[..1], &s[1..])
    } else {
        ("", s)
    };
    Ok(Object::from_str(format!("{sign}{}{rest}", "0".repeat(pad))))
}

fn str_ljust(args: &[Object]) -> Result<Object, RuntimeError> {
    pad_str(args, false)
}

fn str_rjust(args: &[Object]) -> Result<Object, RuntimeError> {
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
    let fill = match args.get(2) {
        Some(Object::Str(f)) if f.chars().count() == 1 => f.chars().next().unwrap(),
        None => ' ',
        _ => return Err(type_error("fill must be single char")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(Object::from_str(s.to_owned()));
    }
    let pad: String = std::iter::repeat_n(fill, width - len).collect();
    Ok(Object::from_str(if right_align {
        format!("{pad}{s}")
    } else {
        format!("{s}{pad}")
    }))
}

fn str_center(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let width = match args.get(1) {
        // Negative widths are no-ops in CPython; clamp to avoid an `as usize`
        // underflow that would request a gigantic allocation.
        Some(Object::Int(i)) => (*i).max(0) as usize,
        Some(Object::Bool(b)) => usize::from(*b),
        _ => return Err(type_error("center() expected int")),
    };
    let fill = match args.get(2) {
        Some(Object::Str(f)) if f.chars().count() == 1 => f.chars().next().unwrap(),
        None => ' ',
        _ => return Err(type_error("fill must be single char")),
    };
    let len = s.chars().count();
    if len >= width {
        return Ok(Object::from_str(s.to_owned()));
    }
    let total = width - len;
    // CPython biases the extra pad to the *left* when both the margin and the
    // width are odd (`marg / 2 + (marg & width & 1)`), so `'Monday'.center(9)`
    // is `'  Monday '`, not `' Monday  '`.
    let left = total / 2 + (total & width & 1);
    let right = total - left;
    let lpad: String = std::iter::repeat_n(fill, left).collect();
    let rpad: String = std::iter::repeat_n(fill, right).collect();
    Ok(Object::from_str(format!("{lpad}{s}{rpad}")))
}

fn str_expandtabs(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let tabsize = match args.get(1) {
        Some(Object::Int(i)) => *i as usize,
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
    Ok(Object::from_str(out))
}

fn str_encode(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let encoding = match args.get(1) {
        Some(Object::Str(e)) => e.to_string(),
        None => "utf-8".to_owned(),
        _ => return Err(type_error("encode() expected str")),
    };
    let errors = match args.get(2) {
        Some(Object::Str(e)) => e.to_string(),
        None => "strict".to_owned(),
        _ => "strict".to_owned(),
    };
    let bytes = crate::stdlib::codecs_mod::encode_str(s, &encoding, &errors)?;
    Ok(Object::new_bytes(bytes))
}

fn str_removeprefix(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let prefix = match args.get(1) {
        Some(Object::Str(p)) => p.to_string(),
        _ => return Err(type_error("removeprefix() expected str")),
    };
    let out = s.strip_prefix(&prefix).unwrap_or(s).to_owned();
    Ok(Object::from_str(out))
}

fn str_removesuffix(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let suffix = match args.get(1) {
        Some(Object::Str(p)) => p.to_string(),
        _ => return Err(type_error("removesuffix() expected str")),
    };
    let out = s.strip_suffix(&suffix).unwrap_or(s).to_owned();
    Ok(Object::from_str(out))
}

fn str_format(args: &[Object]) -> Result<Object, RuntimeError> {
    let template = str_self(args)?.to_owned();
    let rest = &args[1..];
    let kwargs: Vec<(String, Object)> = Vec::new();
    crate::str_format_impl(&template, rest, &kwargs).map(Object::from_str)
}

fn str_format_map(args: &[Object]) -> Result<Object, RuntimeError> {
    let template = str_self(args)?.to_owned();
    let mapping = match args.get(1) {
        Some(Object::Dict(d)) => d.clone(),
        _ => return Err(type_error("format_map() argument must be a mapping")),
    };
    crate::str_format_map_impl(&template, &mapping).map(Object::from_str)
}

fn str_translate(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let table = match args.get(1) {
        Some(Object::Dict(d)) => d.clone(),
        _ => return Err(type_error("translate() argument must be a dict")),
    };
    let mut out = String::new();
    for c in s.chars() {
        let key = DictKey(Object::Int(i64::from(u32::from(c))));
        match table.borrow().get(&key) {
            Some(Object::None) => {}
            Some(Object::Int(i)) => {
                if let Some(ch) = char::from_u32(*i as u32) {
                    out.push(ch);
                }
            }
            Some(Object::Str(s)) => out.push_str(s),
            _ => out.push(c),
        }
    }
    Ok(Object::from_str(out))
}

fn str_maketrans(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut d = DictData::new();
    match args.len() {
        1 => match &args[0] {
            Object::Dict(map) => {
                for (k, v) in map.borrow().iter() {
                    let key = match &k.0 {
                        Object::Str(s) => match s.chars().next() {
                            Some(c) => DictKey(Object::Int(i64::from(u32::from(c)))),
                            None => continue,
                        },
                        Object::Int(_) => k.clone(),
                        _ => return Err(type_error("invalid key in maketrans")),
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
            if let Some(Object::Str(rm)) = args.get(2) {
                for c in rm.chars() {
                    d.insert(DictKey(Object::Int(i64::from(u32::from(c)))), Object::None);
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
        Some(Object::Instance(inst)) => match &inst.native {
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
        crate::apply_slice_assignment(&mut l.borrow_mut(), s, replacement)?;
        return Ok(Object::None);
    }
    let mut l = l.borrow_mut();
    let n = list_index_arg(l.len(), key, "__setitem__")?;
    l[n] = val.clone();
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
            l.remove(i);
        }
        return Ok(Object::None);
    }
    let mut l = l.borrow_mut();
    let n = list_index_arg(l.len(), key, "__delitem__")?;
    l.remove(n);
    Ok(Object::None)
}

fn list_pop(args: &[Object]) -> Result<Object, RuntimeError> {
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
    let mut it = args[1].make_iter()?;
    while let Some(v) = it.next_value() {
        l.borrow_mut().push(v);
    }
    Ok(Object::None)
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
    let mut l = l.borrow_mut();
    let pos = l
        .iter()
        .position(|x| x.eq_value(&args[1]))
        .ok_or_else(|| value_error("list.remove(x): x not in list"))?;
    l.remove(pos);
    Ok(Object::None)
}

fn list_index(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("list.index() expected at least 1 argument"));
    }
    let l = list_self(args)?;
    let l = l.borrow();
    // CPython `list.index(value, start=0, stop=maxsize)`: negative
    // bounds count from the end and clamp to 0 (`PySlice_AdjustIndices`
    // semantics), and the comparison is identity-first
    // (`PyObject_RichCompareBool`).
    let len = l.len() as i64;
    let adjust = |v: i64| -> i64 {
        if v < 0 {
            (v + len).max(0)
        } else {
            v.min(len)
        }
    };
    let start = match args.get(2) {
        Some(o) => adjust(coerce_index_i64(o)?),
        None => 0,
    };
    let stop = match args.get(3) {
        Some(o) => adjust(coerce_index_i64(o)?),
        None => len,
    };
    for i in start..stop {
        let x = &l[i as usize];
        if x.is_same(&args[1]) || x.eq_value(&args[1]) {
            return Ok(Object::Int(i));
        }
    }
    Err(value_error(format!("{} is not in list", args[1].repr())))
}

fn list_count(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.count() expected 1 argument"));
    }
    let l = list_self(args)?;
    let l = l.borrow();
    // CPython compares with `PyObject_RichCompareBool`, which is identity-first,
    // so `[nan].count(nan)` (the same nan) is 1.
    let n = l
        .iter()
        .filter(|x| x.is_same(&args[1]) || x.eq_value(&args[1]))
        .count();
    Ok(Object::Int(n as i64))
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
    list_self(args)?.borrow_mut().reverse();
    Ok(Object::None)
}

fn list_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    list_self(args)?.borrow_mut().clear();
    Ok(Object::None)
}

fn list_copy(args: &[Object]) -> Result<Object, RuntimeError> {
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
        Some(Object::Instance(inst)) => match &inst.native {
            Some(Object::Dict(d)) => Ok(d.clone()),
            _ => Err(type_error("expected dict method receiver")),
        },
        _ => Err(type_error("expected dict method receiver")),
    }
}

fn dict_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.get() expected at least 1 argument"))?;
    let default = args.get(2).cloned().unwrap_or(Object::None);
    let value = d
        .borrow()
        .get(&DictKey(key.clone()))
        .cloned()
        .unwrap_or(default);
    Ok(value)
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
    d.borrow_mut()
        .insert(DictKey(key.clone()), val.clone());
    Ok(Object::None)
}

fn dict_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__getitem__ expected 1 argument"))?;
    let found = d.borrow().get(&DictKey(key.clone())).cloned();
    found.ok_or_else(|| key_error(key.repr()))
}

fn dict_delitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("__delitem__ expected 1 argument"))?;
    if d.borrow_mut().shift_remove(&DictKey(key.clone())).is_some() {
        Ok(Object::None)
    } else {
        Err(key_error(key.repr()))
    }
}

fn dict_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Keys,
    })))
}

fn dict_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Values,
    })))
}

fn dict_items(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    Ok(Object::DictView(Rc::new(crate::object::PyDictView {
        dict: d,
        kind: crate::object::DictViewKind::Items,
    })))
}

fn dict_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.pop() expected at least 1 argument"))?;
    let mut d = d.borrow_mut();
    if let Some(v) = d.shift_remove(&DictKey(key.clone())) {
        Ok(v)
    } else if let Some(default) = args.get(2).cloned() {
        Ok(default)
    } else {
        Err(key_error(key.repr()))
    }
}

fn dict_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    if let Some(other) = args.get(1) {
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
                let mut dst = d.borrow_mut();
                for (k, v) in entries {
                    dst.insert(k, v);
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
                    dst.insert(k, v);
                }
            }
            _ => return Err(type_error("dict.update() expected dict")),
        }
    }
    Ok(Object::None)
}

fn dict_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_self(args)?.borrow_mut().clear();
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
    let n = t
        .iter()
        .filter(|x| x.is_same(&args[1]) || x.eq_value(&args[1]))
        .count();
    Ok(Object::Int(n as i64))
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
        Some(o) => adjust(coerce_index_i64(o)?),
        None => 0,
    };
    let stop = match args.get(3) {
        Some(o) => adjust(coerce_index_i64(o)?),
        None => len,
    };
    for i in start..stop {
        let x = &t[i as usize];
        if x.is_same(&args[1]) || x.eq_value(&args[1]) {
            return Ok(Object::Int(i));
        }
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
    let default = args.get(2).cloned().unwrap_or(Object::None);
    let mut borrowed = d.borrow_mut();
    if let Some(v) = borrowed.get(&key).cloned() {
        return Ok(v);
    }
    borrowed.insert(key, default.clone());
    Ok(default)
}

fn dict_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let cloned = d.borrow().clone();
    Ok(Object::Dict(Rc::new(RefCell::new(cloned))))
}

fn dict_fromkeys(args: &[Object]) -> Result<Object, RuntimeError> {
    // ``fromkeys`` is exposed both as a bound method on a dict
    // (``{}.fromkeys(...)``) and as an unbound classmethod
    // (``dict.fromkeys(...)``). The two call sites have a different
    // shape: the bound version receives the dict as ``args[0]``;
    // the unbound version omits it. Sniff the receiver shape so a
    // single body handles both.
    // A lone dict argument is the *iterable* of an unbound call
    // (`map(dict.fromkeys, list_of_dicts)` — ChainMap.__iter__ does
    // this); a dict in slot 0 only marks the bound receiver when more
    // arguments follow.
    let (it_idx, value_idx) = match (args.first(), args.len()) {
        (Some(Object::Type(_)), _) => (1usize, 2usize),
        (Some(Object::Dict(_)), n) if n >= 2 => (1usize, 2usize),
        _ => (0usize, 1usize),
    };
    let it = args
        .get(it_idx)
        .ok_or_else(|| type_error("fromkeys() expects iterable"))?;
    let value = args.get(value_idx).cloned().unwrap_or(Object::None);
    let mut d = DictData::new();
    let mut iter = it.make_iter()?;
    while let Some(k) = iter.next_value() {
        d.insert(DictKey(k), value.clone());
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

fn dict_popitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let mut borrowed = d.borrow_mut();
    if let Some((k, v)) = borrowed.pop() {
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

fn set_apply_inplace<F: FnOnce(&mut crate::object::SetData)>(
    args: &[Object],
    f: F,
) -> Result<Object, RuntimeError> {
    match set_self(args)? {
        Object::Set(s) => {
            f(&mut s.borrow_mut());
            Ok(Object::None)
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
            let mut it = other.make_iter()?;
            let mut out = Vec::new();
            while let Some(v) = it.next_value() {
                out.push(DictKey(v));
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
    set_apply_inplace(args, |s| {
        s.insert(DictKey(v));
    })
}

fn set_discard(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("discard() expected 1 arg"))?;
    set_apply_inplace(args, |s| {
        s.shift_remove(&DictKey(v));
    })
}

fn set_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("remove() expected 1 arg"))?;
    match set_self(args)? {
        Object::Set(s) => {
            if s.borrow_mut().shift_remove(&DictKey(v.clone())) {
                Ok(Object::None)
            } else {
                Err(key_error(v.repr()))
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
    set_apply_inplace(args, |s| s.clear())
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
        for other in args.iter().skip(1) {
            for k in set_iter_items(other)? {
                s.borrow_mut().insert(k);
            }
        }
    }
    Ok(Object::None)
}

fn set_union(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out = crate::object::SetData::new();
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
    Ok(Object::Set(Rc::new(RefCell::new(out))))
}

fn set_intersection(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut acc = match args.first() {
        Some(first) => {
            let mut s = crate::object::SetData::new();
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
    Ok(Object::Set(Rc::new(RefCell::new(acc))))
}

fn set_difference(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut acc = match args.first() {
        Some(first) => {
            let mut s = crate::object::SetData::new();
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
    Ok(Object::Set(Rc::new(RefCell::new(acc))))
}

fn set_symmetric_difference(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut a: crate::object::SetData = match args.first() {
        Some(first) => set_iter_items(first)?.into_iter().collect(),
        None => return Ok(Object::new_set()),
    };
    let b: crate::object::SetData = match args.get(1) {
        Some(other) => set_iter_items(other)?.into_iter().collect(),
        None => return Ok(Object::Set(Rc::new(RefCell::new(a)))),
    };
    let mut out = crate::object::SetData::new();
    for k in a.iter().filter(|k| !b.contains(*k)) {
        out.insert(k.clone());
    }
    for k in b.iter().filter(|k| !a.contains(*k)) {
        out.insert(k.clone());
    }
    let _ = &mut a;
    Ok(Object::Set(Rc::new(RefCell::new(out))))
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
        for other in args.iter().skip(1) {
            for k in set_iter_items(other)? {
                s.borrow_mut().shift_remove(&k);
            }
        }
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
        let mut out = crate::object::SetData::new();
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

fn bytes_argview(arg: &Object) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::MemoryView(mv) => Ok(mv.to_bytes()),
        Object::Instance(inst) => {
            // bytes/bytearray subclasses carry their payload natively.
            if let Some(native) = &inst.native {
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
        inst @ Object::Instance(_)
            if crate::instance_method(inst, "__index__").is_some() =>
        {
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

fn bytes_decode(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_decode_kw(args, &[])
}

fn bytes_decode_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let encoding = match arg_or_kw(args, 1, kwargs, "encoding") {
        Some(Object::Str(e)) => e.to_string(),
        None => "utf-8".to_owned(),
        _ => return Err(type_error("decode() expected str")),
    };
    let errors = match arg_or_kw(args, 2, kwargs, "errors") {
        Some(Object::Str(e)) => e.to_string(),
        _ => "strict".to_owned(),
    };
    let s = crate::stdlib::codecs_mod::decode_bytes(&data, &encoding, &errors)?;
    Ok(Object::from_str(s))
}

fn bytes_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_hex_kw(args, &[])
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
    let sep: Option<u8> = match &sep_obj {
        None => None,
        Some(Object::Str(s)) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => {
                    if (c as u32) > 0x7f {
                        return Err(value_error("sep must be ASCII."));
                    }
                    Some(c as u8)
                }
                _ => return Err(value_error("sep must be length 1.")),
            }
        }
        Some(Object::Bytes(b)) => {
            if b.len() != 1 {
                return Err(value_error("sep must be length 1."));
            }
            if b[0] > 0x7f {
                return Err(value_error("sep must be ASCII."));
            }
            Some(b[0])
        }
        Some(other) => {
            return Err(type_error(format!(
                "sep must be str or bytes, not {}",
                other.type_name()
            )))
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
    let data = bytes_data(args)?;
    let target = args
        .get(1)
        .ok_or_else(|| type_error("startswith() expected 1 arg"))?;
    let (start, end, invalid) = bytes_search_range(args, data.len());
    if invalid {
        return Ok(Object::Bool(false));
    }
    Ok(Object::Bool(bytes_match_prefix_suffix(
        &data[start..end],
        target,
        true,
    )?))
}

fn bytes_endswith(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_data(args)?;
    let target = args
        .get(1)
        .ok_or_else(|| type_error("endswith() expected 1 arg"))?;
    let (start, end, invalid) = bytes_search_range(args, data.len());
    if invalid {
        return Ok(Object::Bool(false));
    }
    Ok(Object::Bool(bytes_match_prefix_suffix(
        &data[start..end],
        target,
        false,
    )?))
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
/// `__index__`) that mutates the receiving bytearray while the search
/// "holds its buffer". CPython raises `BufferError`; we emulate by
/// snapshotting the length around the conversion.
fn bytes_needle_guarded(args: &[Object], arg: &Object) -> Result<Vec<u8>, RuntimeError> {
    if let Some(Object::ByteArray(cell)) = args.first() {
        let before = cell.borrow().len();
        let sub = bytes_find_needle(arg)?;
        if cell.borrow().len() != before {
            return Err(RuntimeError::PyException(
                crate::error::PyException::from_builtin(
                    "BufferError",
                    "Existing exports of data: object cannot be re-sized",
                ),
            ));
        }
        Ok(sub)
    } else {
        bytes_find_needle(arg)
    }
}

fn bytes_find(args: &[Object]) -> Result<Object, RuntimeError> {
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("find() expected 1 arg"))?,
    )?;
    let data = bytes_data(args)?;
    let (start, end, invalid) = bytes_search_range(args, data.len());
    if invalid {
        return Ok(Object::Int(-1));
    }
    Ok(Object::Int(bytes_find_in(&data, &sub, start, end)))
}

fn bytes_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("rfind() expected 1 arg"))?,
    )?;
    let data = bytes_data(args)?;
    let (start, end, invalid) = bytes_search_range(args, data.len());
    if invalid || end > data.len() {
        return Ok(Object::Int(-1));
    }
    if sub.is_empty() {
        return Ok(Object::Int(end as i64));
    }
    let last = memchr::memmem::rfind(&data[start..end], &sub)
        .map_or(-1, |i| (start + i) as i64);
    Ok(Object::Int(last))
}

fn bytes_index(args: &[Object]) -> Result<Object, RuntimeError> {
    match bytes_find(args)? {
        Object::Int(i) if i >= 0 => Ok(Object::Int(i)),
        _ => Err(value_error("subsection not found")),
    }
}

fn bytes_rindex(args: &[Object]) -> Result<Object, RuntimeError> {
    match bytes_rfind(args)? {
        Object::Int(i) if i >= 0 => Ok(Object::Int(i)),
        _ => Err(value_error("subsection not found")),
    }
}

fn bytes_count(args: &[Object]) -> Result<Object, RuntimeError> {
    let sub = bytes_needle_guarded(
        args,
        args.get(1)
            .ok_or_else(|| type_error("count() expected 1 arg"))?,
    )?;
    let data = bytes_data(args)?;
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
    Ok(bytes_like_result(args, data[start..end].to_vec()))
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
            // converting `sep` can run user code that resizes the
            // receiving bytearray.
            if let Some(Object::ByteArray(cell)) = args.first() {
                let before = cell.borrow().len();
                let sep = bytes_argview(&other)?;
                if cell.borrow().len() != before {
                    return Err(RuntimeError::PyException(
                        crate::error::PyException::from_builtin(
                            "BufferError",
                            "Existing exports of data: object cannot be re-sized",
                        ),
                    ));
                }
                Some(sep)
            } else {
                Some(bytes_argview(&other)?)
            }
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

fn bytes_split_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
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

fn bytes_rsplit_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
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

fn bytes_splitlines(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_splitlines_kw(args, &[])
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

fn bytes_replace(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_replace_kw(args, &[])
}

fn bytes_replace_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
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
fn bytes_translate(args: &[Object]) -> Result<Object, RuntimeError> {
    bytes_translate_kw(args, &[])
}

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

fn bytes_expandtabs(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
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
        _ => Err(type_error(format!("{name}() requires a bytearray receiver"))),
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
        Object::Int(_) | Object::Long(_) => {
            Err(value_error("byte must be in range(0, 256)"))
        }
        inst @ Object::Instance(_)
            if crate::instance_method(inst, "__index__").is_some() =>
        {
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
    Ok(Object::Bool(
        !data.is_empty() && data.iter().all(u8::is_ascii_whitespace),
    ))
}

// ---------- bytearray-only mutators ----------

fn bytearray_self(args: &[Object]) -> Result<Rc<crate::sync::RefCell<Vec<u8>>>, RuntimeError> {
    match args.first() {
        Some(Object::ByteArray(b)) => Ok(b.clone()),
        _ => Err(type_error("expected bytearray receiver")),
    }
}

fn bytearray_append(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let value = args
        .get(1)
        .ok_or_else(|| type_error("append() takes exactly one argument (0 given)"))?;
    let byte = bytearray_byte_arg(value)?;
    b.borrow_mut().push(byte);
    Ok(Object::None)
}

fn bytearray_extend(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    let other = args
        .get(1)
        .ok_or_else(|| type_error("extend() takes exactly 1 argument (0 given)"))?;
    // Bytes-like fast paths (with `b.extend(b)` alias safety).
    match other {
        Object::Bytes(buf) => {
            b.borrow_mut().extend_from_slice(buf);
            return Ok(Object::None);
        }
        Object::ByteArray(buf) => {
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
    // General protocol: any iterable of ints (each through `__index__`,
    // as CPython's `bytearray_extend` does via `_getbytevalue`).
    // Generators and user-`__iter__` objects were materialised by the
    // interpreter's dispatch shim before reaching this builtin.
    let mut it = other.make_iter().map_err(|_| {
        type_error(format!(
            "can't extend bytearray with {}",
            other.type_name()
        ))
    })?;
    // Collect first so a mid-iteration error leaves the target
    // unchanged (CPython builds into a fresh buffer too).
    let mut tmp: Vec<u8> = Vec::new();
    while let Some(item) = it.next_value() {
        tmp.push(bytearray_byte_arg(&item)?);
    }
    b.borrow_mut().extend_from_slice(&tmp);
    Ok(Object::None)
}

fn bytearray_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
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
                return Err(crate::error::index_error("bytearray index out of range"));
            }
            n as usize
        }
        _ => return Err(type_error("pop() index must be int")),
    };
    let v = buf.remove(idx);
    Ok(Object::Int(i64::from(v)))
}

fn bytearray_reverse(args: &[Object]) -> Result<Object, RuntimeError> {
    let b = bytearray_self(args)?;
    b.borrow_mut().reverse();
    Ok(Object::None)
}

// ---------- file methods ----------

fn file_self(args: &[Object]) -> Result<Rc<crate::object::PyFile>, RuntimeError> {
    match args.first() {
        Some(Object::File(f)) => Ok(f.clone()),
        _ => Err(type_error("expected file receiver")),
    }
}

fn file_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let n = match args.get(1) {
        Some(Object::Int(i)) if *i >= 0 => Some(*i as usize),
        None | Some(Object::None) | Some(Object::Int(-1)) => None,
        _ => return Err(type_error("read() argument must be int")),
    };
    let bytes = f.read_bytes(n)?;
    if f.binary {
        Ok(Object::new_bytes(bytes))
    } else {
        let s = String::from_utf8(bytes).map_err(|e| value_error(e.to_string()))?;
        Ok(Object::from_str(s))
    }
}

fn file_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let b = f.read_bytes(Some(1))?;
        if b.is_empty() {
            break;
        }
        out.extend_from_slice(&b);
        if b[0] == b'\n' {
            break;
        }
    }
    if f.binary {
        Ok(Object::new_bytes(out))
    } else {
        let s = String::from_utf8(out).map_err(|e| value_error(e.to_string()))?;
        Ok(Object::from_str(s))
    }
}

/// `next(file)` — return the next line, or raise StopIteration at EOF.
/// Backs both the `__next__` method and the VM's native file iteration.
pub(crate) fn file_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let line = file_readline(args)?;
    let empty = match &line {
        Object::Str(s) => s.is_empty(),
        Object::Bytes(b) => b.is_empty(),
        _ => true,
    };
    if empty {
        Err(stop_iteration())
    } else {
        Ok(line)
    }
}

fn file_readlines(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let mut lines: Vec<Object> = Vec::new();
    loop {
        let line = file_readline(&[Object::File(f.clone())])?;
        let is_empty = match &line {
            Object::Str(s) => s.is_empty(),
            Object::Bytes(b) => b.is_empty(),
            _ => true,
        };
        if is_empty {
            break;
        }
        lines.push(line);
    }
    Ok(Object::new_list(lines))
}

fn file_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let data = args
        .get(1)
        .ok_or_else(|| type_error("write() expected 1 arg"))?;
    let n = match data {
        Object::Str(s) => f.write_bytes(s.as_bytes())?,
        Object::Bytes(b) => f.write_bytes(b)?,
        Object::ByteArray(b) => f.write_bytes(&b.borrow())?,
        _ => return Err(type_error("write() argument must be str or bytes")),
    };
    Ok(Object::Int(n as i64))
}

fn file_writelines(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let it = args
        .get(1)
        .ok_or_else(|| type_error("writelines() expected 1 arg"))?;
    let mut iter = it.make_iter()?;
    while let Some(v) = iter.next_value() {
        match v {
            Object::Str(s) => {
                f.write_bytes(s.as_bytes())?;
            }
            Object::Bytes(b) => {
                f.write_bytes(&b)?;
            }
            _ => return Err(type_error("writelines() item must be str or bytes")),
        }
    }
    Ok(Object::None)
}

fn file_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    file_self(args)?.flush()?;
    Ok(Object::None)
}

fn file_close(args: &[Object]) -> Result<Object, RuntimeError> {
    file_self(args)?.close();
    Ok(Object::None)
}

fn file_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    let offset = match args.get(1) {
        Some(Object::Int(i)) => *i as isize,
        _ => return Err(type_error("seek() expected int")),
    };
    let whence = match args.get(2) {
        Some(Object::Int(i)) => *i as i32,
        None => 0,
        _ => return Err(type_error("seek() whence must be int")),
    };
    Ok(Object::Int(f.seek(offset, whence)? as i64))
}

fn file_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    Ok(Object::Int(f.position() as i64))
}

fn file_getvalue(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = file_self(args)?;
    f.getvalue()
        .ok_or_else(|| type_error("getvalue() requires StringIO/BytesIO"))
}

fn file_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::File(file_self(args)?))
}

fn file_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    file_self(args)?.close();
    Ok(Object::None)
}

// ----- memoryview methods (RFC 0023) -----

fn memoryview_self(args: &[Object]) -> Result<Rc<crate::object::PyMemoryView>, RuntimeError> {
    match args.first() {
        Some(Object::MemoryView(mv)) => Ok(mv.clone()),
        _ => Err(type_error("memoryview method requires a memoryview")),
    }
}

fn memoryview_tobytes(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error("memoryview: released"));
    }
    Ok(Object::Bytes(Rc::from(mv.to_bytes().into_boxed_slice())))
}

fn memoryview_tolist(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if mv.released.get() {
        return Err(value_error("memoryview: released"));
    }
    Ok(Object::new_list(
        mv.to_bytes()
            .into_iter()
            .map(|b| Object::Int(i64::from(b)))
            .collect(),
    ))
}

fn memoryview_release(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    mv.released.set(true);
    Ok(Object::None)
}

fn memoryview_cast(args: &[Object]) -> Result<Object, RuntimeError> {
    let mv = memoryview_self(args)?;
    if let Some(Object::Str(fmt)) = args.get(1) {
        *mv.format.borrow_mut() = fmt.to_string();
        // Itemsize: 1 for B/b, 2 for h/H, 4 for i/I/f, 8 for q/Q/d.
        let item = match fmt.as_ref() {
            "B" | "b" | "c" => 1,
            "h" | "H" => 2,
            "i" | "I" | "f" => 4,
            "q" | "Q" | "d" => 8,
            _ => 1,
        };
        mv.itemsize.set(item);
    }
    Ok(Object::MemoryView(mv))
}

fn memoryview_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::fmt::Write;
    let mv = memoryview_self(args)?;
    let bytes = mv.to_bytes();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in &bytes {
        write!(&mut s, "{b:02x}").expect("write to String");
    }
    Ok(Object::from_str(s))
}

fn memoryview_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args[0].clone())
}

fn memoryview_exit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
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

fn mappingproxy_get(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_get(&mappingproxy_args(args))
}

fn mappingproxy_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_keys(&mappingproxy_args(args))
}

fn mappingproxy_values(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_values(&mappingproxy_args(args))
}

fn mappingproxy_items(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_items(&mappingproxy_args(args))
}

fn mappingproxy_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_copy(&mappingproxy_args(args))
}

fn mappingproxy_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_getitem(&mappingproxy_args(args))
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
