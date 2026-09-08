"""RFC 0068 WS3 — exceptions/mem/immortal/import/watchers/misc C-API
fixture shims.

Python ports of the corresponding `Modules/_testcapi/*.c` and
`Modules/_testlimitedcapi/import.c` fixture wrappers, star-imported by
the frozen `_testcapi` / `_testlimitedcapi` shims. Every exported name
must be listed in `__all__`.

`None` plays the role of C NULL throughout (the tests define
``NULL = None``); lines marked "# CRASHES" in the tests are never
exercised, so NULL combinations that would crash CPython are simply not
handled here.
"""

import sys
import types

__all__ = [
    # --- Modules/_testlimitedcapi/import.c (test_capi.test_import) ---
    "PyImport_GetMagicNumber",
    "PyImport_GetMagicTag",
    "PyImport_GetModuleDict",
    "PyImport_GetModule",
    "PyImport_AddModuleObject",
    "PyImport_AddModule",
    "PyImport_AddModuleRef",
    "PyImport_Import",
    "PyImport_ImportModule",
    "PyImport_ImportModuleNoBlock",
    "PyImport_ImportModuleEx",
    "PyImport_ImportModuleLevel",
    "PyImport_ImportModuleLevelObject",
    "PyImport_ImportFrozenModule",
    "PyImport_ImportFrozenModuleObject",
    "PyImport_ExecCodeModule",
    "PyImport_ExecCodeModuleEx",
    "PyImport_ExecCodeModuleWithPathnames",
    "PyImport_ExecCodeModuleObject",
    # --- Modules/_testcapi/exceptions.c (test_capi.test_exceptions) ---
    "err_set_raised",
    "err_restore",
    "exc_set_object",
    "exc_set_object_fetch",
    "err_setstring",
    "err_setfromerrnowithfilename",
    "err_writeunraisable",
    "set_exception",
    "set_exc_info",
    "function_set_warning",
    "unstable_exc_prep_reraise_star",
    # --- Modules/_testcapi/immortal.c (test_capi.test_immortal) ---
    # (test_immortal_small_ints is provided by _weave_capi_num)
    "test_immortal_builtins",
    "is_immortal",
    "_is_static_immortal",
    # --- Modules/_testcapi/watchers.c (test_capi.test_watchers) ---
    "add_dict_watcher",
    "clear_dict_watcher",
    "watch_dict",
    "unwatch_dict",
    "get_dict_watcher_events",
    "add_type_watcher",
    "clear_type_watcher",
    "watch_type",
    "unwatch_type",
    "get_type_modified_events",
    "add_code_watcher",
    "clear_code_watcher",
    "get_code_watcher_num_created_events",
    "get_code_watcher_num_destroyed_events",
    "allocate_too_many_code_watchers",
    "code_newempty",
    "add_context_watcher",
    "clear_context_watcher",
    "clear_context_stack",
    "get_context_switches",
    "allocate_too_many_context_watchers",
    "add_func_watcher",
    "clear_func_watcher",
    "allocate_too_many_func_watchers",
    "set_func_defaults_via_capi",
    "set_func_kwdefaults_via_capi",
    "PYFUNC_EVENT_CREATE",
    "PYFUNC_EVENT_DESTROY",
    "PYFUNC_EVENT_MODIFY_CODE",
    "PYFUNC_EVENT_MODIFY_DEFAULTS",
    "PYFUNC_EVENT_MODIFY_KWDEFAULTS",
    # --- _testcapimodule.c grab-bag (test_capi.test_misc) ---
    "function_get_code",
    "function_get_globals",
    "function_get_module",
    "function_get_defaults",
    "function_set_defaults",
    "function_get_kw_defaults",
    "function_set_kw_defaults",
    "function_get_closure",
    "function_set_closure",
    "gen_get_code",
    "get_type_name",
    "get_type_qualname",
    "get_type_fullyqualname",
    "get_type_module_name",
    "get_heaptype_for_name",
    "pynumber_tobase",
    "pyobject_repr_from_null",
    "pyobject_str_from_null",
    "pyobject_bytes_from_null",
    "Py_CompileString",
    "clear_managed_dict",
    "type_get_tp_bases",
    "type_get_tp_mro",
    "get_basic_static_type",
    "return_null_without_error",
    "return_result_with_error",
    "getitem_with_error",
    "create_type_from_repeated_slots",
    "make_immutable_type_with_base",
    "HeapCTypeMetaclass",
    "HeapCTypeMetaclassNullNew",
    "HeapCTypeMetaclassCustomNew",
    "pytype_fromspec_meta",
    "make_type_with_base",
    "HeapCCollection",
    "pyobject_getitemdata",
    "make_sized_heaptypes",
    "subclass_var_heaptype",
    "subclass_heaptype",
    "make_heaptype_with_member",
    "make_memoryview_from_NULL_pointer",
    "matmulType",
    "ipowType",
    "MyList",
    "ObjExtraData",
    "HeapDocCType",
    "HeapGcCType",
    "HeapCTypeSubclass",
    "HeapCTypeSubclassWithFinalizer",
    "_test_thread_state",
    "gilstate_ensure_release",
    "test_current_tstate_matches",
    "NullTpDocType",
    "HeapCTypeWithBasesSlot",
    "create_heapctype_with_none_bases_slot",
    "HeapCTypeWithDict",
    "HeapCTypeWithRelativeDict",
    "HeapCTypeWithNegativeDict",
    "HeapCTypeWithManagedDict",
    "HeapCTypeWithWeakref",
    "HeapCTypeWithRelativeWeakref",
    "HeapCTypeWithManagedWeakref",
    "HeapCTypeSetattr",
    "HeapCTypeVectorcall",
    "ManualHeapType",
    "ManagedDictType",
    "HeapCTypeWithBuffer",
    "py_buildvalue",
    "py_buildvalue_ints",
    "test_buildvalue_N",
    "Py_Version",
    "no_docstring",
    "docstring_empty",
    "docstring_no_signature",
    "docstring_with_invalid_signature",
    "docstring_with_invalid_signature2",
    "docstring_with_signature",
    "docstring_with_signature_but_no_doc",
    "docstring_with_signature_and_extra_newlines",
    "DocStringUnrepresentableSignatureTest",
    # --- Modules/_testlimitedcapi/weakref.c and Modules/_testcapi/
    #     weakref.c (test_capi.test_weakref, new in 3.14) ---
    "pyweakref_check",
    "pyweakref_checkref",
    "pyweakref_checkrefexact",
    "pyweakref_checkproxy",
    "pyweakref_newref",
    "pyweakref_newproxy",
    "pyweakref_getref",
    "pyweakref_isdead",
    # --- Modules/_testlimitedcapi/capsule.c (test_capi.test_capsule) ---
    "capsule_new",
    "PyCapsule_Import",
    # --- Modules/_testcapi/codec.c + _testlimitedcapi/codec.c
    #     (test_capi.test_codecs CAPICodecs / CAPICodecErrors) ---
    "codec_register",
    "codec_unregister",
    "codec_known_encoding",
    "codec_encode",
    "codec_decode",
    "codec_encoder",
    "codec_decoder",
    "codec_incremental_encoder",
    "codec_incremental_decoder",
    "codec_stream_reader",
    "codec_stream_writer",
    "codec_register_error",
    "codec_lookup_error",
    "codec_strict_errors",
    "codec_ignore_errors",
    "codec_replace_errors",
    "codec_xmlcharrefreplace_errors",
    "codec_backslashreplace_errors",
    "codec_namereplace_errors",
    # --- Modules/_testcapi/exceptions.c: PyUnicode*Error_{Get,Set}{Start,End}
    #     (test_capi.test_exceptions.TestUnicodeError) ---
    "unicode_encode_get_start",
    "unicode_decode_get_start",
    "unicode_translate_get_start",
    "unicode_encode_set_start",
    "unicode_decode_set_start",
    "unicode_translate_set_start",
    "unicode_encode_get_end",
    "unicode_decode_get_end",
    "unicode_translate_get_end",
    "unicode_encode_set_end",
    "unicode_decode_set_end",
    "unicode_translate_set_end",
]


# =====================================================================
# PyCapsule_Import (Modules/_testlimitedcapi/capsule.c)
# =====================================================================

def _charp(name):
    # The "z#" convention: str is passed through, bytes are the raw
    # char* (decoded as UTF-8 by the API that consumes them), None is
    # NULL.
    if name is None or isinstance(name, str):
        return name
    if isinstance(name, (bytes, bytearray)):
        return bytes(name)
    raise TypeError(
        "argument must be str, bytes or None, not %s" % type(name).__name__
    )


def capsule_new(name):
    """`capsule_new(name)`: a capsule whose pointer is a copy of its own
    name (so a successful PyCapsule_Import round-trips the name); a
    None name is a NULL name with a non-NULL dummy pointer."""
    import _weave_capsule
    name = _charp(name)
    cap = _weave_capsule.new_capsule(name)
    object.__setattr__(cap, "_capsule_pointer",
                       name if name is not None else b"")
    return cap


def _capsule_fields(obj):
    import _weave_capsule
    if type(obj) is not _weave_capsule.PyCapsule:
        return None
    try:
        name = object.__getattribute__(obj, "_capsule_name")
    except AttributeError:
        name = None
    try:
        pointer = object.__getattribute__(obj, "_capsule_pointer")
    except AttributeError:
        pointer = None
    return name, pointer


def _capsule_name_bytes(name):
    if name is None:
        return None
    if isinstance(name, str):
        return name.encode("utf-8", "surrogateescape")
    return bytes(name)


def PyCapsule_Import(name, no_block=0):
    """Port of PyCapsule_Import (Objects/capsule.c): the first
    dotted component is imported, the rest are attribute lookups, and
    the resulting object must be a capsule whose name matches the
    whole dotted path. `no_block` is ignored, as in CPython."""
    name = _charp(name)
    if name is None:
        raise SystemError("bad argument to internal function")
    raw = _capsule_name_bytes(name)
    # strchr()-style split on the raw bytes, one component at a time.
    obj = None
    parts = raw.split(b".")
    for i, part in enumerate(parts):
        if i == 0:
            try:
                modname = part.decode("utf-8")
            except UnicodeDecodeError:
                modname = None
            mod = None
            if modname is not None:
                try:
                    mod = __import__(modname)
                    # __import__ returns the top-level package; walk to
                    # the dotted target like PyImport_ImportModule does.
                    for sub in modname.split(".")[1:]:
                        mod = getattr(mod, sub)
                except BaseException as e:
                    if isinstance(e, (KeyboardInterrupt, SystemExit)):
                        raise
                    mod = None
            if mod is None:
                shown = part.decode("utf-8", "backslashreplace")
                raise ImportError(
                    'PyCapsule_Import could not import module "%s"' % shown
                )
            obj = mod
        else:
            # PyObject_GetAttrString: the C string must be UTF-8.
            attr = part.decode("utf-8")
            obj = getattr(obj, attr)
    fields = _capsule_fields(obj)
    if fields is not None:
        cap_name, pointer = fields
        cap_raw = _capsule_name_bytes(cap_name)
        if pointer is not None and cap_raw is not None and cap_raw == raw:
            if isinstance(pointer, bytes):
                return pointer.decode("utf-8")
            return pointer
    shown = raw.decode("utf-8", "backslashreplace")
    raise AttributeError('PyCapsule_Import "%s" is not valid' % shown)


# =====================================================================
# PyCodec_* wrappers (Modules/_testcapi/codec.c, _testlimitedcapi/codec.c)
# =====================================================================

def _bad_argument():
    raise TypeError("bad argument type for built-in operation")


def _codec_lookup(encoding):
    # _PyCodec_Lookup: a NULL encoding is PyErr_BadArgument().
    import codecs
    if encoding is None:
        _bad_argument()
    return codecs.lookup(encoding)


def codec_register(search_function):
    import codecs
    codecs.register(search_function)
    return None


def codec_unregister(search_function):
    import codecs
    codecs.unregister(search_function)
    return None


def codec_known_encoding(encoding):
    # PyCodec_KnownEncoding: a failed lookup is False, never an error.
    import codecs
    if encoding is None:
        return False
    try:
        codecs.lookup(encoding)
    except LookupError:
        return False
    return True


def codec_encode(input, encoding=None, errors=None):
    # PyCodec_Encode(input, encoding, errors)
    import codecs
    if input is None:
        raise TypeError("bad argument type for built-in operation")
    info = _codec_lookup(encoding)
    result = info.encode(input, errors if errors is not None else "strict")
    if not isinstance(result, tuple) or len(result) != 2:
        raise TypeError("encoder must return a tuple (object, integer)")
    return result[0]


def codec_decode(input, encoding=None, errors=None):
    # PyCodec_Decode(input, encoding, errors)
    if input is None:
        raise TypeError("bad argument type for built-in operation")
    info = _codec_lookup(encoding)
    result = info.decode(input, errors if errors is not None else "strict")
    if not isinstance(result, tuple) or len(result) != 2:
        raise TypeError("decoder must return a tuple (object,integer)")
    return result[0]


def codec_encoder(encoding):
    return _codec_lookup(encoding).encode


def codec_decoder(encoding):
    return _codec_lookup(encoding).decode


def _codec_getincrementalcodec(encoding, errors, attr):
    factory = getattr(_codec_lookup(encoding), attr)
    if errors is not None:
        return factory(errors)
    return factory()


def codec_incremental_encoder(encoding, errors):
    return _codec_getincrementalcodec(encoding, errors, "incrementalencoder")


def codec_incremental_decoder(encoding, errors):
    return _codec_getincrementalcodec(encoding, errors, "incrementaldecoder")


def _codec_getstreamcodec(encoding, stream, errors, attr):
    factory = getattr(_codec_lookup(encoding), attr)
    if errors is not None:
        return factory(stream, errors)
    return factory(stream)


def codec_stream_reader(encoding, stream, errors):
    return _codec_getstreamcodec(encoding, stream, errors, "streamreader")


def codec_stream_writer(encoding, stream, errors):
    return _codec_getstreamcodec(encoding, stream, errors, "streamwriter")


def codec_register_error(encoding, error):
    import codecs
    if not isinstance(encoding, str):
        raise TypeError(
            "argument 1 must be str, not %s" % type(encoding).__name__
        )
    codecs.register_error(encoding, error)
    return None


def codec_lookup_error(name):
    # PyCodec_LookupError(NULL) is the strict handler.
    import codecs
    if name is None:
        name = "strict"
    return codecs.lookup_error(name)


def codec_strict_errors(exc):
    import codecs
    return codecs.strict_errors(exc)


def codec_ignore_errors(exc):
    import codecs
    return codecs.ignore_errors(exc)


def codec_replace_errors(exc):
    import codecs
    return codecs.replace_errors(exc)


def codec_xmlcharrefreplace_errors(exc):
    import codecs
    return codecs.xmlcharrefreplace_errors(exc)


def codec_backslashreplace_errors(exc):
    import codecs
    return codecs.backslashreplace_errors(exc)


def codec_namereplace_errors(exc):
    import codecs
    return codecs.namereplace_errors(exc)


# =====================================================================
# PyUnicode{Encode,Decode,Translate}Error_{Get,Set}{Start,End}
# (Objects/exceptions.c via Modules/_testcapi/exceptions.c)
# =====================================================================

def _unicode_error_check(exc, exc_type):
    if exc is None:
        raise SystemError("bad argument to internal function")
    if not isinstance(exc, exc_type):
        raise TypeError(
            "expected %s, got %s" % (exc_type.__name__, type(exc).__name__)
        )


def _unicode_error_object(exc, exc_type):
    obj = exc.object
    if exc_type is UnicodeDecodeError:
        if not isinstance(obj, (bytes, bytearray)):
            raise TypeError("object attribute must be bytes")
    elif not isinstance(obj, str):
        raise TypeError("object attribute must be unicode")
    return obj


def _unicode_error_get_start(exc, exc_type):
    # start is clamped to [0, max(0, len - 1)].
    _unicode_error_check(exc, exc_type)
    obj = _unicode_error_object(exc, exc_type)
    start = exc.start
    if not isinstance(start, int):
        raise TypeError("start attribute must be int")
    n = len(obj)
    if start < 0:
        return 0
    if n == 0:
        return 0
    return min(start, n - 1)


def _unicode_error_get_end(exc, exc_type):
    # end is clamped to [min(1, len), max(min(1, len), len)].
    _unicode_error_check(exc, exc_type)
    obj = _unicode_error_object(exc, exc_type)
    end = exc.end
    if not isinstance(end, int):
        raise TypeError("end attribute must be int")
    n = len(obj)
    lo = min(1, n)
    if end < lo:
        return lo
    return min(end, max(lo, n))


def _unicode_error_set(exc, exc_type, attr, value):
    _unicode_error_check(exc, exc_type)
    if not isinstance(value, int):
        raise TypeError("an integer is required")
    setattr(exc, attr, int(value))
    return None


def unicode_encode_get_start(exc):
    return _unicode_error_get_start(exc, UnicodeEncodeError)


def unicode_decode_get_start(exc):
    return _unicode_error_get_start(exc, UnicodeDecodeError)


def unicode_translate_get_start(exc):
    return _unicode_error_get_start(exc, UnicodeTranslateError)


def unicode_encode_set_start(exc, start):
    return _unicode_error_set(exc, UnicodeEncodeError, "start", start)


def unicode_decode_set_start(exc, start):
    return _unicode_error_set(exc, UnicodeDecodeError, "start", start)


def unicode_translate_set_start(exc, start):
    return _unicode_error_set(exc, UnicodeTranslateError, "start", start)


def unicode_encode_get_end(exc):
    return _unicode_error_get_end(exc, UnicodeEncodeError)


def unicode_decode_get_end(exc):
    return _unicode_error_get_end(exc, UnicodeDecodeError)


def unicode_translate_get_end(exc):
    return _unicode_error_get_end(exc, UnicodeTranslateError)


def unicode_encode_set_end(exc, end):
    return _unicode_error_set(exc, UnicodeEncodeError, "end", end)


def unicode_decode_set_end(exc, end):
    return _unicode_error_set(exc, UnicodeDecodeError, "end", end)


def unicode_translate_set_end(exc, end):
    return _unicode_error_set(exc, UnicodeTranslateError, "end", end)


# =====================================================================
# PyWeakref_* wrappers (Modules/_testlimitedcapi/weakref.c,
# Modules/_testcapi/weakref.c)
# =====================================================================

def _weakref_types():
    import weakref
    return (weakref.ReferenceType, weakref.ProxyType,
            weakref.CallableProxyType)


def pyweakref_check(obj):
    # PyWeakref_Check(): any of the three weakref types (subclasses too).
    return 1 if isinstance(obj, _weakref_types()) else 0


def pyweakref_checkref(obj):
    # PyWeakref_CheckRef()
    import weakref
    return 1 if isinstance(obj, weakref.ReferenceType) else 0


def pyweakref_checkrefexact(obj):
    # PyWeakref_CheckRefExact()
    import weakref
    return 1 if type(obj) is weakref.ReferenceType else 0


def pyweakref_checkproxy(obj):
    # PyWeakref_CheckProxy()
    import weakref
    return 1 if isinstance(obj, (weakref.ProxyType,
                                 weakref.CallableProxyType)) else 0


def pyweakref_newref(obj, callback=None):
    # PyWeakref_NewRef(); a None callback is the NULL callback.
    import weakref
    if callback is None:
        return weakref.ref(obj)
    return weakref.ref(obj, callback)


def pyweakref_newproxy(obj, callback=None):
    # PyWeakref_NewProxy()
    import weakref
    if callback is None:
        return weakref.proxy(obj)
    return weakref.proxy(obj, callback)


def _weakref_referent(ref):
    # Shared by PyWeakref_GetRef / PyWeakref_IsDead: NULL is a
    # SystemError, a non-weakref a TypeError; proxies are unwrapped
    # through the underlying reference.
    import weakref
    if ref is None:
        raise SystemError("bad argument to internal function")
    # `type(ref)` rather than isinstance(): a dead proxy forwards
    # `__class__` to its referent and would raise ReferenceError.
    if not issubclass(type(ref), _weakref_types()):
        raise TypeError("expected a weakref")
    # Every weakref flavour (ref or proxy) carries the same per-target
    # deref slot; the ReferenceType-level `__call__` reads it without
    # going through the proxy's attribute forwarding.
    return weakref.ReferenceType.__call__(ref)


def pyweakref_getref(ref):
    # PyWeakref_GetRef(): 0 when dead, (1, obj) when alive.
    obj = _weakref_referent(ref)
    if obj is None:
        return 0
    return (1, obj)


def pyweakref_isdead(ref):
    # PyWeakref_IsDead()
    return 0 if _weakref_referent(ref) is not None else 1


# =====================================================================
# PyImport_* wrappers (Modules/_testlimitedcapi/import.c)
# =====================================================================

def _decode_charp(name):
    # The C wrappers take names with the "z#" convention: str passes
    # through, bytes are the raw char* and get decoded as UTF-8 by the
    # PyImport_* API itself (UnicodeDecodeError for e.g. b'\xff').
    if isinstance(name, (bytes, bytearray)):
        return bytes(name).decode("utf-8")
    return name


def PyImport_GetMagicNumber():
    import importlib.util

    return int.from_bytes(importlib.util.MAGIC_NUMBER, "little")


def PyImport_GetMagicTag():
    return sys.implementation.cache_tag


def PyImport_GetModuleDict():
    return sys.modules


def PyImport_GetModule(name):
    # The C fixture maps a NULL result without error to PyExc_KeyError.
    try:
        return sys.modules[name]  # unhashable name -> TypeError
    except KeyError:
        return KeyError


def PyImport_AddModuleObject(name):
    # import_add_module: reuse an existing module entry, else create a
    # fresh module and register it. Any hashable name object works.
    modules = sys.modules
    try:
        mod = modules[name]
    except KeyError:
        mod = None
    if mod is not None and isinstance(mod, types.ModuleType):
        return mod
    # PyModule_NewObject stores the name object as-is (it need not be a
    # str), which types.ModuleType() would reject — set it afterwards.
    mod = types.ModuleType("tmp")
    mod.__name__ = name
    mod.__doc__ = None
    modules[name] = mod
    return mod


def PyImport_AddModule(name):
    return PyImport_AddModuleObject(_decode_charp(name))


def PyImport_AddModuleRef(name):
    return PyImport_AddModuleObject(_decode_charp(name))


def PyImport_Import(name):
    if name is None:
        raise SystemError("null import name")
    # PyImport_Import calls __import__(name, g, g, ["__doc__"], 0) and
    # re-fetches the module from sys.modules.
    __import__(name, None, None, ["__doc__"], 0)
    return sys.modules[name]


def PyImport_ImportModule(name):
    return PyImport_Import(_decode_charp(name))


def PyImport_ImportModuleNoBlock(name):
    import warnings

    warnings.warn(
        "PyImport_ImportModuleNoBlock() is deprecated and scheduled for "
        "removal in Python 3.15. Use PyImport_ImportModule() instead.",
        DeprecationWarning,
        stacklevel=2,
    )
    return PyImport_Import(_decode_charp(name))


def _import_module_level(name, globals, locals, fromlist, level):
    if level < 0:
        raise ValueError("level must be >= 0")
    package = None
    if level > 0:
        # Pre-apply the resolve_name() checks WeavePy's __import__ maps
        # to the wrong exception types.
        if globals is None:
            raise KeyError("'__name__' not in globals")
        if not isinstance(globals, dict):
            raise TypeError("globals must be a dict")
        package = globals.get("__package__")
        spec = globals.get("__spec__")
        if package is None and spec is not None:
            package = spec.parent
        if package is None:
            import warnings

            warnings.warn(
                "can't resolve package from __spec__ or __package__, "
                "falling back on __name__ and __path__",
                ImportWarning,
                stacklevel=3,
            )
            if "__name__" not in globals:
                raise KeyError("'__name__' not in globals")
            package = globals["__name__"]
            if "__path__" not in globals:
                package = package.rpartition(".")[0]
    mod = __import__(name, globals, locals, fromlist, level)
    if fromlist or level == 0 or not name:
        return mod
    # Empty fromlist + relative import: CPython's __import__ returns the
    # imported submodule cut at the first dot of *name* (i.e. the full
    # submodule for an undotted name). WeavePy's __import__ returns the
    # top-level package instead, so recompute the right module.
    bits = package.rsplit(".", level - 1)
    if len(bits) < level:
        raise ImportError("attempted relative import beyond top-level "
                          "package")
    abs_name = "%s.%s" % (bits[0], name)
    cut_off = len(name) - len(name.partition(".")[0])
    return sys.modules.get(abs_name[:len(abs_name) - cut_off], mod)


def PyImport_ImportModuleLevelObject(name, globals, locals, fromlist, level):
    if name is None:
        raise ValueError("Empty module name")
    if not isinstance(name, str):
        raise TypeError("module name must be a string")
    return _import_module_level(name, globals, locals, fromlist, level)


def PyImport_ImportModuleLevel(name, globals, locals, fromlist, level):
    return PyImport_ImportModuleLevelObject(
        _decode_charp(name), globals, locals, fromlist, level)


def PyImport_ImportModuleEx(name, globals, locals, fromlist):
    return PyImport_ImportModuleLevelObject(
        _decode_charp(name), globals, locals, fromlist, 0)


def PyImport_ImportFrozenModuleObject(name):
    if name is None or not isinstance(name, str):
        return 0
    import _imp

    if not name or not _imp.is_frozen(name):
        return 0
    code = _imp.get_frozen_object(name)
    if code is None:
        try:
            from importlib.machinery import FrozenImporter

            code = FrozenImporter.get_code(name)
        except Exception:
            code = None
    if code is None:
        return 0
    # Execute the frozen code in the (existing or new) module entry,
    # like PyImport_ImportFrozenModuleObject does.
    module = PyImport_AddModuleObject(name)
    exec(code, module.__dict__)
    return 1


def PyImport_ImportFrozenModule(name):
    return PyImport_ImportFrozenModuleObject(_decode_charp(name))


def _fsdecode_charp(path):
    if path is None:
        return None
    if isinstance(path, (bytes, bytearray)):
        import os

        return os.fsdecode(bytes(path))
    return path


def PyImport_ExecCodeModuleObject(name, code, pathname=None, cpathname=None):
    import builtins
    import importlib._bootstrap_external as _external

    module = PyImport_AddModuleObject(name)
    d = module.__dict__
    if "__builtins__" not in d:
        d["__builtins__"] = builtins
    if pathname is None:
        pathname = code.co_filename
    else:
        # CPython 3.13's spec_from_file_location() makes the location
        # absolute; WeavePy's vendored bootstrap does not, so do it here.
        import os

        pathname = os.path.abspath(pathname)
    _external._fix_up_module(d, name, pathname, cpathname)
    exec(code, d)
    try:
        return sys.modules[name]
    except KeyError:
        raise ImportError(
            "Loaded module %r not found in sys.modules" % (name,),
            name=name) from None


def PyImport_ExecCodeModuleWithPathnames(name, code, pathname=None,
                                         cpathname=None):
    name = _decode_charp(name)
    cpathname = _fsdecode_charp(cpathname)
    pathname = _fsdecode_charp(pathname)
    if pathname is None and cpathname is not None:
        import importlib._bootstrap_external as _external

        try:
            pathname = _external._get_sourcefile(cpathname)
        except Exception:
            pathname = None
    return PyImport_ExecCodeModuleObject(name, code, pathname, cpathname)


def PyImport_ExecCodeModuleEx(name, code, pathname=None):
    return PyImport_ExecCodeModuleWithPathnames(name, code, pathname, None)


def PyImport_ExecCodeModule(name, code):
    return PyImport_ExecCodeModuleWithPathnames(name, code, None, None)


# =====================================================================
# PyErr_* wrappers (Modules/_testcapi/exceptions.c)
# =====================================================================

def _pyerr_normalize(exc, value):
    """`_PyErr_SetObject`'s normalization: return the exception
    *instance* the C call would leave as the raised exception."""
    if not (isinstance(exc, type) and issubclass(exc, BaseException)):
        return SystemError(
            "_PyErr_SetObject: "
            "exception %r is not a BaseException subclass" % (exc,))
    if isinstance(value, BaseException):
        # PyObject_IsSubclass goes through the metaclass, so a broken
        # __subclasscheck__ propagates (the tests rely on this).
        if issubclass(type(value), exc):
            return value
        wrap_single = True
    else:
        wrap_single = False
    try:
        if value is None:
            return exc()
        if not wrap_single and isinstance(value, tuple):
            return exc(*value)
        return exc(value)
    except BaseException as creation_exc:
        err = creation_exc
        try:
            args_repr = repr(value)
        except BaseException:
            args_repr = "<unknown>"
        try:
            err.add_note("Normalization failed: type=%s args=%s"
                         % (exc.__name__, args_repr))
        except BaseException:
            pass
        return err


def err_set_raised(exc):
    # PyErr_SetRaisedException(exc): raise exactly this instance.
    raise exc


def err_restore(*args):
    # PyErr_Restore(type[, value[, tb]]).
    if not 1 <= len(args) <= 3:
        raise TypeError("wrong number of arguments")
    typ = args[0]
    value = args[1] if len(args) >= 2 else None
    tb = args[2] if len(args) >= 3 else None
    if tb is not None and not isinstance(tb, types.TracebackType):
        raise TypeError("traceback must be a Traceback or None")
    exc = _pyerr_normalize(typ, value)
    if tb is not None:
        exc = exc.with_traceback(tb)
    raise exc


def exc_set_object(exc, obj):
    # PyErr_SetObject(exc, obj) and return NULL: i.e. raise.
    raise _pyerr_normalize(exc, obj)


def exc_set_object_fetch(exc, obj):
    # PyErr_SetObject + PyErr_Fetch: return the normalized exception
    # value instead of raising it.
    return _pyerr_normalize(exc, obj)


def err_setstring(exc, value):
    # PyErr_SetString(exc, msg): msg arrives as a C char* (bytes).
    if isinstance(value, (bytes, bytearray)):
        value = bytes(value).decode("utf-8")
    raise _pyerr_normalize(exc, value)


def err_setfromerrnowithfilename(error, exc, value):
    # PyErr_SetFromErrnoWithFilename(exc, filename) with errno=error.
    import os

    msg = os.strerror(error) if error else "Error"
    if value is None:
        args = (error, msg)
    else:
        if isinstance(value, (bytes, bytearray)):
            value = os.fsdecode(bytes(value))
        args = (error, msg, value)
    raise _pyerr_normalize(exc, args)


class _UnraisableArgsMisc:
    """The shape sys.unraisablehook receives (UnraisableHookArgs)."""

    __slots__ = ("exc_type", "exc_value", "exc_traceback", "err_msg",
                 "object")

    def __init__(self, exc_type, exc_value, exc_traceback, err_msg, obj):
        self.exc_type = exc_type
        self.exc_value = exc_value
        self.exc_traceback = exc_traceback
        self.err_msg = err_msg
        self.object = obj


def _default_unraisable_write(args):
    # _PyErr_WriteUnraisableDefaultHook / write_unraisable_exc_file.
    import traceback as _tbmod

    stderr = getattr(sys, "stderr", None)
    if stderr is None:
        return
    obj = args.object
    err_msg = args.err_msg
    if obj is not None:
        if err_msg is not None:
            stderr.write("%s: " % (err_msg,))
        else:
            stderr.write("Exception ignored in: ")
        try:
            stderr.write(repr(obj))
        except BaseException:
            stderr.write("<object repr() failed>")
        stderr.write("\n")
    elif err_msg is not None:
        stderr.write("%s:\n" % (err_msg,))
    tb = args.exc_traceback
    if tb is not None:
        # PyTraceBack_Print: header + frame lines, no caret decoration.
        text = "Traceback (most recent call last):\n" + "".join(
            _tbmod.format_tb(tb))
        kept = [
            line
            for line in text.splitlines()
            if line.strip() and not set(line.strip()) <= set("^~")
        ]
        stderr.write("\n".join(kept) + "\n")
    exc_type = args.exc_type
    if exc_type is None:
        return
    modname = getattr(exc_type, "__module__", None)
    if isinstance(modname, str) and modname not in ("builtins", "__main__"):
        stderr.write(modname + ".")
    qualname = getattr(exc_type, "__qualname__", None)
    stderr.write(qualname if isinstance(qualname, str) else "<unknown>")
    value = args.exc_value
    if value is not None:
        try:
            text = str(value)
        except BaseException:
            text = "<exception str() failed>"
        stderr.write(": " + text)
    stderr.write("\n")
    try:
        stderr.flush()
    except BaseException:
        pass


def _fire_unraisable(exc, err_msg, obj, frame):
    """format_unraisable_v: route exc through sys.unraisablehook (or the
    default writer), synthesizing a traceback from *frame* when the
    exception carries none — that is how the C API reports the caller's
    line number."""
    tb = getattr(exc, "__traceback__", None)
    if tb is None and frame is not None:
        try:
            tb = types.TracebackType(None, frame, frame.f_lasti,
                                     frame.f_lineno)
            exc.__traceback__ = tb
        except BaseException:
            tb = None
    args = _UnraisableArgsMisc(type(exc), exc, tb, err_msg, obj)
    hook = getattr(sys, "unraisablehook", None)
    if hook is None or hook is getattr(sys, "__unraisablehook__", None):
        _default_unraisable_write(args)
        return
    audit = getattr(sys, "audit", None)
    if audit is not None:
        try:
            audit("sys.unraisablehook", hook, args)
        except BaseException:
            pass
    try:
        hook(args)
    except BaseException as hook_exc:
        args2 = _UnraisableArgsMisc(
            type(hook_exc), hook_exc,
            getattr(hook_exc, "__traceback__", None),
            None, hook)
        _default_unraisable_write(args2)


def err_writeunraisable(exc, obj):
    # PyErr_SetRaisedException(exc); PyErr_WriteUnraisable(obj)
    if exc is not None:
        _fire_unraisable(exc, None, obj, sys._getframe(1))


def set_exception(new_exc):
    # PyErr_GetHandledException / PyErr_SetHandledException swap the
    # thread's *handled* exception (sys.exception()). WeavePy keeps
    # that state inside the interpreter with no mutation hook reachable
    # from Python code.
    import unittest

    raise unittest.SkipTest(
        "WeavePy: PyErr_SetHandledException requires native support "
        "(cannot mutate sys.exception() from Python code)")


def set_exc_info(new_type, new_value, new_tb):
    # PyErr_GetExcInfo / PyErr_SetExcInfo: same limitation as above.
    import unittest

    raise unittest.SkipTest(
        "WeavePy: PyErr_SetExcInfo requires native support "
        "(cannot mutate sys.exc_info() from Python code)")


def function_set_warning():
    # PyErr_WarnEx(PyExc_RuntimeWarning, "Testing PyErr_WarnEx", 2): the
    # C fixture attributes the warning to the *caller* of the Python
    # function invoking it; this shim adds one extra frame, so use
    # stacklevel 3.
    try:
        import warnings
    except ImportError:
        # Interpreter finalization: CPython's C warning machinery can't
        # import `warnings` either and falls back to the bare
        # "<file>:<line>: <Category>: <msg>" line (no source line) on
        # stderr — test_exceptions.test_warn_during_finalization.
        frame = sys._getframe(2)
        sys.stderr.write(
            "%s:%d: RuntimeWarning: Testing PyErr_WarnEx\n"
            % (frame.f_code.co_filename, frame.f_lineno)
        )
        return

    warnings.warn("Testing PyErr_WarnEx", RuntimeWarning, 3)


# --- PyUnstable_Exc_PrepReraiseStar (Objects/exceptions.c) -----------

def _collect_leaf_ids(exc, leaf_ids):
    if exc is None:
        return
    if isinstance(exc, BaseExceptionGroup):
        for e in exc.exceptions:
            _collect_leaf_ids(e, leaf_ids)
    else:
        leaf_ids.add(id(exc))


def _exception_group_projection(eg, keep):
    leaf_ids = set()
    for e in keep:
        _collect_leaf_ids(e, leaf_ids)
    match, _rest = eg.split(lambda e: id(e) in leaf_ids)
    return match


def _is_same_exception_metadata(exc, orig):
    return (getattr(exc, "__notes__", None) is getattr(orig, "__notes__",
                                                       None)
            and exc.__traceback__ is orig.__traceback__
            and exc.__cause__ is orig.__cause__
            and exc.__context__ is orig.__context__)


def unstable_exc_prep_reraise_star(orig, excs):
    if orig is None or not isinstance(orig, BaseException):
        raise TypeError("orig must be an exception instance")
    if excs is None or not isinstance(excs, list):
        raise TypeError("excs must be a list of exception instances")
    for i, e in enumerate(excs):
        if not (e is None or isinstance(e, BaseException)):
            raise TypeError("item %d of excs is not an exception" % i)
    if orig.__traceback__ is None:
        raise ValueError("orig must be a raised exception")
    # _PyExc_PrepReraiseStar
    if len(excs) == 0:
        return None
    if not isinstance(orig, BaseExceptionGroup):
        return excs[0]
    raised = []
    reraised = []
    for e in excs:
        if e is None:
            continue
        if _is_same_exception_metadata(e, orig):
            reraised.append(e)
        else:
            raised.append(e)
    reraised_eg = _exception_group_projection(orig, reraised)
    if not raised:
        return reraised_eg
    if reraised_eg is not None:
        raised.append(reraised_eg)
    if len(raised) > 1:
        return BaseExceptionGroup("", raised)
    return raised[0]


# =====================================================================
# Immortality probes (Modules/_testcapi/immortal.c)
# =====================================================================

def _refcount_pinned(obj):
    getrefcount = sys.getrefcount
    before = getrefcount(obj)
    refs = [obj] * 10000
    during = getrefcount(obj)
    del refs
    return before == during == getrefcount(obj)


class _CFuncMisc:
    """Builtin-function stand-in: callable but *not* a descriptor.
    Needed because test_misc's Test_testcapi/Test_testlimitedcapi
    classes hoist every module-level `test_*` attribute into a test
    method — a plain function would wrongly bind `self`."""

    def __init__(self, func):
        self._func = func
        self.__name__ = func.__name__

    def __call__(self, *args, **kwargs):
        return self._func(*args, **kwargs)

    def __repr__(self):
        return "<built-in function %s>" % self.__name__


@_CFuncMisc
def _statically_immortal(obj):
    # The objects CPython allocates in the static data segment (and so
    # marks immortal, `_Py_IsStaticImmortal`): the singletons, the small
    # ints, the empty tuple/bytes/str, the latin-1 single characters, and
    # every static builtin type.
    if obj is None or obj is True or obj is False:
        return True
    if obj is Ellipsis or obj is NotImplemented:
        return True
    t = type(obj)
    if t is int:
        return -5 <= obj <= 256
    if t is tuple or t is bytes:
        return len(obj) == 0 or (t is bytes and len(obj) == 1)
    if t is str:
        return len(obj) == 0 or (len(obj) == 1 and ord(obj) < 256)
    if t is type:
        return getattr(obj, "__module__", None) == "builtins"
    return False


def is_immortal(obj):
    """`PyUnstable_IsImmortal(obj)` (test_capi.test_immortal)."""
    return _statically_immortal(obj)


def _is_static_immortal(obj):
    """`_Py_IsStaticImmortal(obj)`, behind `_testinternalcapi`."""
    return _statically_immortal(obj)


@_CFuncMisc
def test_immortal_builtins():
    # verify_immortality(): an immortal object's refcount must not
    # respond to new references being created and destroyed.
    for obj in (True, False, None):
        if not _refcount_pinned(obj):
            raise AssertionError("%r is not immortal" % (obj,))
    if not _refcount_pinned(...):
        import unittest

        raise unittest.SkipTest(
            "WeavePy: Ellipsis is a reference-counted singleton, not an "
            "immortal object; needs native immortality support")


# =====================================================================
# Watcher fixtures (Modules/_testcapi/watchers.c)
#
# WeavePy has no native dict/type/func mutation-event hooks, so the
# EVENTS-kind watchers can register and validate IDs (all the C error
# paths) but never observe mutations — those test legs need native
# support. Code-object events *are* dispatched faithfully because the
# only code objects the tests watch are the ones our own code_newempty
# creates (creation is direct, destruction via weakref callback).
# =====================================================================

_DICT_MAX_WATCHERS = 8
_TYPE_MAX_WATCHERS = 8
_CODE_MAX_WATCHERS = 8
_FUNC_MAX_WATCHERS = 8

_dict_watchers = [None] * _DICT_MAX_WATCHERS
_dict_watch_events = None
_dict_watchers_installed = 0

_type_watchers = [None] * _TYPE_MAX_WATCHERS
# 3.14 reserves type watcher slot 0 for the interpreter's own
# specialization cache (`TYPE_WATCHER_FOR_...` in pycore_typeobject.h),
# so fixture watchers start at ID 1 (test_watchers.TestTypeWatchers
# test_error pins "#1").
_type_watchers[0] = "interp"
_type_modified_events = None
_type_watchers_installed = 0


def _allocate_watcher_slot(slots, kind, exhausted_msg):
    for i, k in enumerate(slots):
        if k is None:
            slots[i] = kind
            return i
    raise RuntimeError(exhausted_msg)


def _validate_watcher_id(slots, watcher_id, what):
    watcher_id = int(watcher_id)
    if watcher_id < 0 or watcher_id >= len(slots):
        raise ValueError("Invalid %s watcher ID %d" % (what, watcher_id))
    if slots[watcher_id] is None:
        raise ValueError("No %s watcher set for ID %d" % (what, watcher_id))
    return watcher_id


def _watch_natives():
    import _testinternalcapi

    return _testinternalcapi


_DICT_EVENT_NAMES = {
    "ADDED": "PyDict_EVENT_ADDED",
    "MODIFIED": "PyDict_EVENT_MODIFIED",
    "DELETED": "PyDict_EVENT_DELETED",
    "CLONED": "PyDict_EVENT_CLONED",
    "CLEARED": "PyDict_EVENT_CLEARED",
    "DEALLOCATED": "PyDict_EVENT_DEALLOCATED",
}


def _dispatch_dict_watch_event(event, mask, addr, key, value):
    # Native dispatch trampoline: `event` is the PyDict_WatchEvent name
    # suffix, `mask` the bitmask of watcher IDs watching this dict,
    # `addr` the dict's allocation address (for the unraisable message).
    for wid in range(_DICT_MAX_WATCHERS):
        if not (mask >> wid) & 1:
            continue
        kind = _dict_watchers[wid]
        if kind is None:
            continue
        if kind == 1:  # ERROR: raises; routed to unraisable like the C hook
            _fire_unraisable(
                RuntimeError("boom!"),
                "Exception ignored in %s watcher callback for <dict at 0x%x>"
                % (_DICT_EVENT_NAMES.get(event, event), addr),
                None,
                None,
            )
            continue
        if _dict_watch_events is None:
            continue
        if kind == 2:  # SECOND
            _dict_watch_events.append("second")
        elif event == "ADDED":
            _dict_watch_events.append("new:%s:%s" % (key, value))
        elif event == "MODIFIED":
            _dict_watch_events.append("mod:%s:%s" % (key, value))
        elif event == "DELETED":
            _dict_watch_events.append("del:%s" % (key,))
        elif event == "CLONED":
            _dict_watch_events.append("clone")
        elif event == "CLEARED":
            _dict_watch_events.append("clear")
        elif event == "DEALLOCATED":
            _dict_watch_events.append("dealloc")


def add_dict_watcher(kind):
    global _dict_watch_events, _dict_watchers_installed
    wid = _allocate_watcher_slot(_dict_watchers, int(kind),
                                 "no more dict watcher IDs available")
    if not _dict_watchers_installed:
        _dict_watch_events = []
    _dict_watchers_installed += 1
    return wid


def clear_dict_watcher(watcher_id):
    global _dict_watch_events, _dict_watchers_installed
    wid = _validate_watcher_id(_dict_watchers, watcher_id, "dict")
    _dict_watchers[wid] = None
    _watch_natives()._clear_dict_watcher(wid)
    _dict_watchers_installed -= 1
    if not _dict_watchers_installed:
        _dict_watch_events = None


def watch_dict(watcher_id, d):
    wid = _validate_watcher_id(_dict_watchers, watcher_id, "dict")
    if not isinstance(d, dict):
        raise ValueError("Cannot watch non-dictionary")
    _watch_natives()._watch_dict(wid, d)


def unwatch_dict(watcher_id, d):
    wid = _validate_watcher_id(_dict_watchers, watcher_id, "dict")
    if not isinstance(d, dict):
        raise ValueError("Cannot watch non-dictionary")
    _watch_natives()._unwatch_dict(wid, d)


def get_dict_watcher_events():
    if _dict_watch_events is None:
        raise RuntimeError("no watchers active")
    return _dict_watch_events


def _dispatch_type_watch_event(mask, tp):
    for wid in range(_TYPE_MAX_WATCHERS):
        if not (mask >> wid) & 1:
            continue
        kind = _type_watchers[wid]
        if kind is None or kind == "interp":
            continue
        if kind == 1:  # ERROR
            _fire_unraisable(
                RuntimeError("boom!"),
                "Exception ignored in type watcher callback #%d for %r"
                % (wid, tp),
                None,
                None,
            )
            continue
        if _type_modified_events is None:
            continue
        if kind == 2:  # WRAP
            _type_modified_events.append([tp])
        else:  # TYPES
            _type_modified_events.append(tp)


def add_type_watcher(kind):
    global _type_modified_events, _type_watchers_installed
    wid = _allocate_watcher_slot(_type_watchers, int(kind),
                                 "no more type watcher IDs available")
    if not _type_watchers_installed:
        _type_modified_events = []
    _type_watchers_installed += 1
    return wid


def clear_type_watcher(watcher_id):
    global _type_modified_events, _type_watchers_installed
    wid = _validate_watcher_id(_type_watchers, watcher_id, "type")
    _type_watchers[wid] = None
    _watch_natives()._clear_type_watcher(wid)
    _type_watchers_installed -= 1
    if not _type_watchers_installed:
        _type_modified_events = None


def watch_type(watcher_id, tp):
    wid = _validate_watcher_id(_type_watchers, watcher_id, "type")
    if not isinstance(tp, type):
        raise ValueError("Cannot watch non-type")
    _watch_natives()._watch_type(wid, tp)


def unwatch_type(watcher_id, tp):
    wid = _validate_watcher_id(_type_watchers, watcher_id, "type")
    if not isinstance(tp, type):
        raise ValueError("Cannot watch non-type")
    _watch_natives()._unwatch_type(wid, tp)


def get_type_modified_events():
    if _type_modified_events is None:
        raise RuntimeError("no watchers active")
    return _type_modified_events


# --- code object watchers --------------------------------------------

# slot value: 0/1 = counting fixture watcher, 2 = error watcher,
# "noop" = allocate_too_many filler.
_code_watchers = [None] * _CODE_MAX_WATCHERS
_code_created = [0, 0]
_code_destroyed = [0, 0]
_live_code_refs = set()


def add_code_watcher(which_watcher):
    which = int(which_watcher)
    if which not in (0, 1, 2):
        raise ValueError("invalid watcher %d" % which)
    wid = _allocate_watcher_slot(_code_watchers, which,
                                 "no more code watcher IDs available")
    if which in (0, 1):
        _code_created[which] = 0
        _code_destroyed[which] = 0
    return wid


def clear_code_watcher(watcher_id):
    wid = _validate_watcher_id(_code_watchers, watcher_id, "code")
    which = _code_watchers[wid]
    _code_watchers[wid] = None
    if which in (0, 1):
        _code_created[which] = 0
        _code_destroyed[which] = 0


def get_code_watcher_num_created_events(which):
    return _code_created[int(which)]


def get_code_watcher_num_destroyed_events(which):
    return _code_destroyed[int(which)]


def _dispatch_code_event(event, co_repr):
    for which in _code_watchers:
        if which is None or which == "noop":
            continue
        if which in (0, 1):
            if event == "CREATE":
                _code_created[which] += 1
            else:
                _code_destroyed[which] += 1
        elif which == 2:
            err = RuntimeError("boom!")
            _fire_unraisable(
                err,
                "Exception ignored in PY_CODE_EVENT_%s watcher callback "
                "for %s" % (event, co_repr),
                None,
                None,
            )


def code_newempty(filename, funcname, firstlineno):
    # PyCode_NewEmpty(filename, funcname, firstlineno)
    import weakref

    co = compile("", filename, "exec").replace(
        co_name=funcname, co_firstlineno=firstlineno)
    _dispatch_code_event("CREATE", repr(co))
    co_repr = "<code object %s>" % funcname

    ref = None

    def _on_dealloc(_ref):
        _live_code_refs.discard(ref)
        _dispatch_code_event("DESTROY", co_repr)

    ref = weakref.ref(co, _on_dealloc)
    _live_code_refs.add(ref)
    return co


def allocate_too_many_code_watchers():
    allocated = []
    exc = None
    try:
        for _ in range(_CODE_MAX_WATCHERS + 1):
            allocated.append(
                _allocate_watcher_slot(
                    _code_watchers, "noop",
                    "no more code watcher IDs available"))
    except RuntimeError as e:
        exc = e
    for wid in allocated:
        _code_watchers[wid] = None
    if exc is not None:
        raise exc


# --- context watchers (3.14) -------------------------------------------

_CONTEXT_MAX_WATCHERS = 8
_NUM_CONTEXT_WATCHERS = 2
# slot value: 0/1 = recording fixture watcher, 2 = error watcher,
# "noop" = allocate_too_many filler.
_context_watchers = [None] * _CONTEXT_MAX_WATCHERS
_context_watcher_ids = [-1, -1]
_context_switches = [None, None]


def _dispatch_context_switch(ctx):
    for wid, which in enumerate(_context_watchers):
        if which is None or which == "noop":
            continue
        if which == 2:
            _fire_unraisable(
                RuntimeError("boom!"),
                "Exception ignored in Py_CONTEXT_SWITCHED watcher callback "
                "for %r" % (ctx,),
                None,
                None,
            )
        elif _context_switches[which] is not None:
            _context_switches[which].append(ctx)


def _sync_context_hook():
    import contextvars

    active = any(w is not None and w != "noop" for w in _context_watchers)
    contextvars._set_switch_hook(_dispatch_context_switch if active else None)


def add_context_watcher(which_watcher):
    which = int(which_watcher)
    if which < 0 or which > 2:
        raise ValueError("invalid watcher %d" % which)
    wid = _allocate_watcher_slot(_context_watchers, which,
                                 "no more context watcher IDs available")
    if which < _NUM_CONTEXT_WATCHERS:
        _context_watcher_ids[which] = wid
        _context_switches[which] = []
    _sync_context_hook()
    return wid


def clear_context_watcher(watcher_id):
    wid = _validate_watcher_id(_context_watchers, watcher_id, "context")
    _context_watchers[wid] = None
    for i in range(_NUM_CONTEXT_WATCHERS):
        if _context_watcher_ids[i] == wid:
            _context_watcher_ids[i] = -1
            _context_switches[i] = None
    _sync_context_hook()


def clear_context_stack():
    import contextvars

    contextvars._clear_context_stack()


def get_context_switches(watcher_id):
    which = int(watcher_id)
    if which < 0 or which >= _NUM_CONTEXT_WATCHERS:
        raise ValueError("invalid watcher %d" % which)
    if _context_switches[which] is None:
        return []
    return _context_switches[which]


def allocate_too_many_context_watchers():
    allocated = []
    exc = None
    try:
        for _ in range(_CONTEXT_MAX_WATCHERS + 1):
            allocated.append(
                _allocate_watcher_slot(
                    _context_watchers, "noop",
                    "no more context watcher IDs available"))
    except RuntimeError as e:
        exc = e
    for wid in allocated:
        _context_watchers[wid] = None
    if exc is not None:
        raise exc


# --- function watchers ------------------------------------------------

PYFUNC_EVENT_CREATE = 0
PYFUNC_EVENT_DESTROY = 1
PYFUNC_EVENT_MODIFY_CODE = 2
PYFUNC_EVENT_MODIFY_DEFAULTS = 3
PYFUNC_EVENT_MODIFY_KWDEFAULTS = 4

_func_watchers = [None] * _FUNC_MAX_WATCHERS
_test_func_watcher_slots = [None, None]  # (watcher_id, callback) pairs


_FUNC_EVENT_IDS = {
    "CREATE": PYFUNC_EVENT_CREATE,
    "DESTROY": PYFUNC_EVENT_DESTROY,
    "MODIFY_CODE": PYFUNC_EVENT_MODIFY_CODE,
    "MODIFY_DEFAULTS": PYFUNC_EVENT_MODIFY_DEFAULTS,
    "MODIFY_KWDEFAULTS": PYFUNC_EVENT_MODIFY_KWDEFAULTS,
}


def _dispatch_func_watch_event(event, func, new_value):
    ev = _FUNC_EVENT_IDS[event]
    arg = id(func) if ev == PYFUNC_EVENT_DESTROY else func
    if ev in (PYFUNC_EVENT_CREATE, PYFUNC_EVENT_DESTROY):
        new_value = None
    for cb in list(_func_watchers):
        if cb is None or cb == "noop":
            continue
        try:
            cb(ev, arg, new_value)
        except BaseException as exc:
            _fire_unraisable(
                exc,
                "Exception ignored in PyFunction_EVENT_%s watcher "
                "callback for %s" % (event, repr(func)[1:-1]),
                None,
                None,
            )


def _sync_func_watchers_active():
    active = any(cb is not None and cb != "noop" for cb in _func_watchers)
    _watch_natives()._set_func_watchers_active(active)


def add_func_watcher(func):
    if not isinstance(func, types.FunctionType):
        raise TypeError("'func' must be a function")
    for idx in range(len(_test_func_watcher_slots)):
        if _test_func_watcher_slots[idx] is None:
            break
    else:
        raise RuntimeError("no free test watchers")
    for wid in range(_FUNC_MAX_WATCHERS):
        if _func_watchers[wid] is None:
            _func_watchers[wid] = func
            break
    else:
        raise RuntimeError("no more func watcher IDs available")
    _test_func_watcher_slots[idx] = (wid, func)
    _sync_func_watchers_active()
    return wid


def clear_func_watcher(watcher_id):
    wid = int(watcher_id)
    if wid < 0 or wid >= _FUNC_MAX_WATCHERS:
        raise ValueError("invalid func watcher ID %d" % wid)
    if _func_watchers[wid] is None:
        raise ValueError("no func watcher set for ID %d" % wid)
    _func_watchers[wid] = None
    for idx, entry in enumerate(_test_func_watcher_slots):
        if entry is not None and entry[0] == wid:
            _test_func_watcher_slots[idx] = None
    _sync_func_watchers_active()


def allocate_too_many_func_watchers():
    allocated = []
    exc = None
    try:
        for _ in range(_FUNC_MAX_WATCHERS + 1):
            for wid in range(_FUNC_MAX_WATCHERS):
                if _func_watchers[wid] is None:
                    _func_watchers[wid] = "noop"
                    allocated.append(wid)
                    break
            else:
                raise RuntimeError("no more func watcher IDs available")
    except RuntimeError as e:
        exc = e
    for wid in allocated:
        _func_watchers[wid] = None
    if exc is not None:
        raise exc


def set_func_defaults_via_capi(func, defaults):
    # PyFunction_SetDefaults(func, defaults)
    if not isinstance(func, types.FunctionType):
        raise SystemError("PyFunction_SetDefaults: not a function")
    if defaults is not None and not isinstance(defaults, tuple):
        raise SystemError("non-tuple default args")
    func.__defaults__ = defaults


def set_func_kwdefaults_via_capi(func, kwdefaults):
    # PyFunction_SetKwDefaults(func, kwdefaults)
    if not isinstance(func, types.FunctionType):
        raise SystemError("PyFunction_SetKwDefaults: not a function")
    if kwdefaults is not None and not isinstance(kwdefaults, dict):
        raise SystemError("non-dict keyword arguments")
    func.__kwdefaults__ = kwdefaults


# =====================================================================
# test_misc grab-bag fixtures (_testcapimodule.c and friends)
# =====================================================================

def _check_function(func, api):
    if not isinstance(func, types.FunctionType):
        raise SystemError("%s: not a function" % api)


def function_get_code(func):
    _check_function(func, "PyFunction_GetCode")
    return func.__code__


def function_get_globals(func):
    _check_function(func, "PyFunction_GetGlobals")
    return func.__globals__


def function_get_module(func):
    _check_function(func, "PyFunction_GetModule")
    return func.__module__


def function_get_defaults(func):
    _check_function(func, "PyFunction_GetDefaults")
    return func.__defaults__


def function_set_defaults(func, defaults):
    if not isinstance(func, types.FunctionType):
        raise SystemError("PyFunction_SetDefaults: not a function")
    if defaults is not None and not isinstance(defaults, tuple):
        raise SystemError("non-tuple default args")
    if defaults is not None and type(defaults) is not tuple:
        # PyFunction_SetDefaults accepts tuple subclasses; WeavePy's
        # __defaults__ setter requires an exact tuple, so coerce (the
        # tests compare by equality, not identity).
        defaults = tuple(defaults)
    func.__defaults__ = defaults


def function_get_kw_defaults(func):
    _check_function(func, "PyFunction_GetKwDefaults")
    return func.__kwdefaults__


def function_set_kw_defaults(func, kwdefaults):
    if not isinstance(func, types.FunctionType):
        raise SystemError("PyFunction_SetKwDefaults: not a function")
    if kwdefaults is not None and not isinstance(kwdefaults, dict):
        raise SystemError("non-dict keyword arguments")
    if kwdefaults is not None and type(kwdefaults) is not dict:
        # See function_set_defaults: coerce dict subclasses.
        kwdefaults = dict(kwdefaults)
    func.__kwdefaults__ = kwdefaults


def function_get_closure(func):
    _check_function(func, "PyFunction_GetClosure")
    return func.__closure__


def function_set_closure(func, closure):
    if not isinstance(func, types.FunctionType):
        raise SystemError("PyFunction_SetClosure: not a function")
    if closure is not None and not isinstance(closure, tuple):
        raise SystemError("expected tuple for closure, got '%s'"
                          % type(closure).__name__)
    # PyFunction_SetClosure mutates func_closure in place; __closure__
    # is a read-only attribute in WeavePy (as in CPython), so the
    # mutation itself needs native support.
    import unittest

    raise unittest.SkipTest(
        "WeavePy: PyFunction_SetClosure requires native support "
        "(__closure__ is read-only from Python code)")


def gen_get_code(gen):
    if not isinstance(gen, types.GeneratorType):
        raise SystemError("gen_get_code: not a generator object")
    return gen.gi_code


def get_type_name(tp):
    # PyType_GetName
    return tp.__name__


def get_type_qualname(tp):
    # PyType_GetQualName
    return tp.__qualname__


def get_type_fullyqualname(tp):
    # PyType_GetFullyQualifiedName: "<module>.<qualname>", with the
    # module omitted when missing, non-str, "builtins" or "__main__"
    # (CPython 3.13 Objects/typeobject.c _PyType_GetFullyQualifiedName).
    # Note: running test_misc directly as a script makes the earlier
    # MyType subtest expect an "__main__." prefix, which contradicts
    # this rule — CPython itself only passes that file under regrtest's
    # import mode, where __name__ is "test.test_capi.test_misc".
    qualname = tp.__qualname__
    module = getattr(tp, "__module__", None)
    if not isinstance(module, str) or module in ("builtins", "__main__"):
        return qualname
    return "%s.%s" % (module, qualname)


def get_type_module_name(tp):
    # PyType_GetModuleName
    return tp.__module__


def get_heaptype_for_name():
    # A heap type living in the fixture module's namespace.
    return type("HeapTypeNameType", (), {"__module__": "_testcapi"})


def pynumber_tobase(n, base):
    # PyNumber_ToBase(n, base)
    if base not in (2, 8, 10, 16):
        raise SystemError("PyNumber_ToBase: base must be 2, 8, 10 or 16")
    import operator

    n = operator.index(n)
    if base == 2:
        return bin(n)
    if base == 8:
        return oct(n)
    if base == 16:
        return hex(n)
    return str(n)


def pyobject_repr_from_null():
    # PyObject_Repr(NULL)
    return "<NULL>"


def pyobject_str_from_null():
    # PyObject_Str(NULL)
    return "<NULL>"


def pyobject_bytes_from_null():
    # PyObject_Bytes(NULL)
    return b"<NULL>"


def Py_CompileString(source):
    # Py_CompileString(source, "<string>", Py_file_input): compiling
    # from bytes respects the PEP 263 coding cookie.
    return compile(source, "<string>", "exec")


def clear_managed_dict(obj):
    # PyObject_ClearManagedDict(obj)
    obj.__dict__.clear()


def type_get_tp_bases(tp):
    # Reads PyTypeObject.tp_bases directly; only checked for non-NULL.
    return tp.__bases__


def type_get_tp_mro(tp):
    # Reads PyTypeObject.tp_mro directly; only checked for non-NULL.
    return tp.__mro__


def get_basic_static_type(base=None):
    # BasicStaticTypes: freshly readied static types with/without a
    # preset tp_base. Emulated with a plain new type per call.
    if base is None:
        base = object
    return type("BasicStaticType", (base,), {})


def return_null_without_error():
    # In non-debug builds _Py_CheckFunctionResult turns a NULL return
    # with no exception set into this SystemError.
    raise SystemError("<built-in function return_null_without_error> "
                      "returned NULL without setting an exception")


def return_result_with_error():
    # Non-debug _Py_CheckFunctionResult: result + exception set.
    raise SystemError("<built-in function return_result_with_error> "
                      "returned a result with an exception set") \
        from ValueError()


def getitem_with_error(mapping, key):
    # Sets ValueError("bug"), then calls PyObject_GetItem(); non-debug
    # builds report it through _Py_CheckFunctionResult.
    raise SystemError("<built-in function getitem_with_error> "
                      "returned a result with an exception set") \
        from ValueError("bug")


def create_type_from_repeated_slots(variant):
    # PyType_FromSpec rejects duplicate slot IDs.
    raise SystemError("Multiple slots with the same id are not allowed")


class _ImmutableMeta(type):
    """Emulates Py_TPFLAGS_IMMUTABLETYPE attribute protection."""

    def __setattr__(cls, name, value):
        raise TypeError("cannot set %r attribute of immutable type %r"
                        % (name, cls.__name__))

    def __delattr__(cls, name):
        raise TypeError("cannot delete %r attribute of immutable type %r"
                        % (name, cls.__name__))


def make_immutable_type_with_base(base):
    # 3.14: an immutable heap type may not derive from a mutable base
    # (the 3.12/3.13 DeprecationWarning became the TypeError).
    if not (base.__flags__ & (1 << 8)):  # Py_TPFLAGS_IMMUTABLETYPE
        raise TypeError(
            "Creating immutable type ImmutableSubclass from mutable base %s"
            % base.__name__
        )
    return _ImmutableMeta("ImmutableSubclass", (base,), {})


class HeapCTypeMetaclass(type):
    """Metaclass whose tp_new is inherited from type."""


class HeapCTypeMetaclassNullNew(type):
    """Metaclass with tp_new = NULL."""

    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create '%s.%s' instances"
                        % (cls.__module__, cls.__qualname__))


class HeapCTypeMetaclassCustomNew(type):
    """Metaclass with a custom tp_new."""

    _weave_custom_tp_new_ = True

    def __new__(cls, name, bases, namespace, **kwargs):
        return super().__new__(cls, name, bases, namespace, **kwargs)


def pytype_fromspec_meta(meta):
    # PyType_FromMetaclass(meta, ..., spec of "HeapCTypeViaMetaclass").
    if not (isinstance(meta, type) and issubclass(meta, type)):
        raise TypeError("metaclass must inherit from type")
    if getattr(meta, "_weave_custom_tp_new_", False):
        raise TypeError("Metaclasses with custom tp_new are not supported.")
    # Bypass meta.__new__ like PyType_FromMetaclass bypasses tp_new.
    cls = type.__new__(meta, "HeapCTypeViaMetaclass", (object,), {})
    type.__init__(cls, "HeapCTypeViaMetaclass", (object,), {})
    return cls


def make_type_with_base(base):
    # PyType_FromSpecWithBases with a custom-tp_new metaclass: deprecated
    # in 3.12/3.13, a TypeError since 3.14 (typeobject.c
    # `PyType_FromMetaclass`).
    if getattr(type(base), "_weave_custom_tp_new_", False):
        raise TypeError("Metaclasses with custom tp_new are not supported.")
    return type(base)("Subclass", (base,), {"__module__": "_testcapi"})


# PY_VERSION_HEX baked into the C module at compile time.
Py_Version = sys.hexversion


def _skip_needs_native(what):
    import unittest

    raise unittest.SkipTest("WeavePy: %s requires native support" % what)


class HeapCCollection(tuple):
    """`HeapCCollection` (_testcapimodule.c): a var-sized heap type
    holding its constructor arguments as items."""

    def __new__(cls, *items):
        return super().__new__(cls, items)


def pyobject_getitemdata(obj):
    # PyObject_GetItemData requires Py_TPFLAGS_ITEMS_AT_END.
    raise TypeError(
        "type '%s' does not have Py_TPFLAGS_ITEMS_AT_END"
        % type(obj).__name__)


def make_sized_heaptypes(extra_base_size, basicsize):
    # PEP 697 relative basicsize layout arithmetic on raw C structs.
    _skip_needs_native("PyType_FromMetaclass with relative basicsize")


def subclass_var_heaptype(base, basicsize, itemsize, offset):
    _skip_needs_native("PyType_FromMetaclass with relative basicsize")


def subclass_heaptype(base, basicsize, itemsize):
    _skip_needs_native("PyType_FromMetaclass with relative basicsize")


# `structmember.h` / `typeslots` constants the 3.14 heap-type legs pass.
Py_TP_USE_SPEC = 0
Py_T_PYSSIZET = 19
Py_READONLY = 1
Py_RELATIVE_OFFSET = 8

def make_heaptype_with_member(extra_base_size=0, basicsize=0, offset=0,
                              relative=False, *, add_relative_flag=None,
                              member_name="memb", member_offset=None,
                              member_type=Py_T_PYSSIZET,
                              member_flags=Py_READONLY):
    # The 3.14 special-member leg passes keywords; run CPython's
    # `type_ready`/`PyType_FromMetaclass` validation for the relative
    # special offsets (`__dictoffset__` & co.) before deferring the
    # real struct-member type to native support.
    if add_relative_flag is not None:
        relative = add_relative_flag
    if member_offset is not None:
        offset = member_offset
    if relative and basicsize > 0:
        raise SystemError(
            "With Py_RELATIVE_OFFSET, basicsize must be negative.")
    if relative and (offset < 0 or offset >= -basicsize):
        raise SystemError("Member offset out of range (0..-basicsize)")
    if member_name in ("__vectorcalloffset__", "__dictoffset__",
                       "__weaklistoffset__"):
        if member_type != Py_T_PYSSIZET:
            raise SystemError(
                "type of %s must be Py_T_PYSSIZET" % member_name)
        if member_flags != Py_READONLY:
            raise SystemError(
                "flags for %s must be Py_READONLY" % member_name)
    _skip_needs_native("Py_RELATIVE_OFFSET struct members")


def make_memoryview_from_NULL_pointer():
    # PyMemoryView_FromBuffer with a NULL buf pointer.
    raise ValueError(
        "cannot make memory view from a buffer with a NULL data pointer")


class matmulType:
    """`matmulType` (_testcapimodule.c): records @ and @= dispatch."""

    def __matmul__(self, other):
        return ("matmul", self, other)

    def __rmatmul__(self, other):
        return ("matmul", other, self)

    def __imatmul__(self, other):
        return ("imatmul", self, other)


class ipowType:
    """`ipowType`: three-argument __ipow__ (gh issue regression)."""

    def __ipow__(self, other, mod=None):
        return (other, mod)


class MyList(list):
    """`MyList` (_testcapimodule.c): list subclass whose deallocation
    exercises deeply nested container teardown (trashcan probes)."""


class ObjExtraData:
    """`ObjExtraData`: PyUnstable_Object_{Get,Set}ExtraData carrier —
    the `extra` slot reads back None once cleared."""

    __slots__ = ("_extra",)

    @property
    def extra(self):
        try:
            return self._extra
        except AttributeError:
            return None

    @extra.setter
    def extra(self, value):
        self._extra = value

    @extra.deleter
    def extra(self):
        try:
            del self._extra
        except AttributeError:
            pass


class HeapDocCType:
    """somedoc"""

    # The C type's Py_tp_doc was "HeapDocCType(arg1, arg2)\n--\n\nsomedoc";
    # type creation strips the signature out of `__doc__` and serves it
    # through the `__text_signature__` getset. The out-of-band key is the
    # VM's channel for exactly that split.
    __weavepy_text_signature__ = "(arg1, arg2)"


class NullTpDocType:
    pass


# `class NullTpDocType: pass` would inherit no docstring already, but be
# explicit that tp_doc is NULL:
NullTpDocType.__doc__ = None


class HeapGcCType:
    """Heap GC type whose tp_init stores value=10 (Modules/_testcapi/
    heaptype.c heapgcctype_init)."""

    def __init__(self):
        self.value = 10


class HeapCTypeSubclass(HeapGcCType):
    """C subclass whose tp_init stores value2=20 then chains to the
    base init (heapctypesubclass_init)."""

    def __init__(self):
        self.value2 = 20
        super().__init__()


class HeapCTypeSubclassWithFinalizer(HeapCTypeSubclass):
    """tp_finalize reassigns __class__ to HeapCTypeSubclass and records
    both types' refcounts (heapctypesubclasswithfinalizer_finalize)."""

    def __init__(self):
        super().__init__()

    def __del__(self):
        self.__class__ = HeapCTypeSubclass
        HeapCTypeSubclass.refcnt_in_del = sys.getrefcount(HeapCTypeSubclass)
        HeapCTypeSubclassWithFinalizer.refcnt_in_del = sys.getrefcount(
            HeapCTypeSubclassWithFinalizer)


class HeapCTypeWithBasesSlot(int):
    """A heap type whose Py_tp_bases slot names (int,)."""


def create_heapctype_with_none_bases_slot():
    raise SystemError("Py_tp_bases is not a tuple")


def _check_solid_layout_conflict(cls):
    """CPython's best_base() rejects multiple bases with conflicting
    solid (non-managed) instance layouts; WeavePy does not enforce this
    for emulated heap types, so reproduce the check here."""
    tags = set()
    for base in cls.__bases__:
        tag = getattr(base, "_weave_solid_layout_", None)
        if tag is not None:
            tags.add(tag)
        elif issubclass(base, (list, tuple, dict, set, str, bytes, int,
                               float)):
            tags.add("builtin")
    if len(tags) > 1:
        raise TypeError("multiple bases have instance lay-out conflict")


class HeapCTypeWithDict:
    """Heap type with a tp_dictoffset dict plus a `dictobj` member (a
    solid layout, unlike Py_TPFLAGS_MANAGED_DICT)."""

    _weave_solid_layout_ = "dict"

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _check_solid_layout_conflict(cls)

    @property
    def dictobj(self):
        return self.__dict__


class HeapCTypeWithRelativeDict:
    """3.14 _testlimitedcapi twin of HeapCTypeWithDict: the dict slot is
    declared with Py_RELATIVE_OFFSET (`Py_T_OBJECT_EX` member at a
    basicsize-relative offset), still a solid layout."""

    _weave_solid_layout_ = "dict"

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _check_solid_layout_conflict(cls)

    @property
    def dictobj(self):
        return self.__dict__


class HeapCTypeWithNegativeDict:
    _weave_solid_layout_ = "negative_dict"

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _check_solid_layout_conflict(cls)

    @property
    def dictobj(self):
        return self.__dict__


class HeapCTypeWithManagedDict:
    """Py_TPFLAGS_MANAGED_DICT: behaves like a plain Python class."""


class HeapCTypeWithWeakref:
    _weave_solid_layout_ = "weakref"

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _check_solid_layout_conflict(cls)

    @property
    def weakreflist(self):
        return getattr(self, "__weakref__", None)


class HeapCTypeWithRelativeWeakref:
    """3.14 _testlimitedcapi twin of HeapCTypeWithWeakref (the weaklist
    slot declared with Py_RELATIVE_OFFSET)."""

    _weave_solid_layout_ = "weakref"

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _check_solid_layout_conflict(cls)

    @property
    def weakreflist(self):
        return getattr(self, "__weakref__", None)


class HeapCTypeWithManagedWeakref:
    """Py_TPFLAGS_MANAGED_WEAKREF: plain Python class semantics."""


class _VectorcallMeta(type):
    # `tp()` goes through tp_vectorcall (value = 1); the explicit
    # `tp.__new__(tp)` + `__init__()` path is tp_new/tp_init (value = 2).
    def __call__(cls, *args, **kwargs):
        if cls is HeapCTypeVectorcall and not args and not kwargs:
            self = cls.__new__(cls)
            self.value = 1
            return self
        return super().__call__(*args, **kwargs)


class HeapCTypeVectorcall(metaclass=_VectorcallMeta):
    """Heap type with a Py_tp_vectorcall slot (3.14
    heaptype_with_tp_vectorcall)."""

    def __init__(self):
        self.value = 2


class ManualHeapType:
    """gh-128923: a heap type allocated and initialised by hand
    (PyType_GenericAlloc + PyType_Ready) rather than PyType_FromSpec."""


class ManagedDictType:
    """Py_TPFLAGS_MANAGED_DICT type built from a spec: `__dict__` is
    readable and assignable (PyObject_GenericGetDict/SetDict)."""


class HeapCTypeSetattr:
    """Heap type with a custom tp_setattro storing the 'value'
    attribute into the `pvalue` member (deleting resets it to 0)."""

    __slots__ = ("pvalue",)

    def __init__(self):
        object.__setattr__(self, "pvalue", 10)

    def __setattr__(self, name, value):
        if name == "value":
            object.__setattr__(self, "pvalue", value)
        else:
            object.__setattr__(self, name, value)

    def __delattr__(self, name):
        if name == "value":
            object.__setattr__(self, "pvalue", 0)
        else:
            object.__delattr__(self, name)


class HeapCTypeWithBuffer:
    """Heap type exporting a fixed b"1234" buffer."""

    def __buffer__(self, flags):
        return memoryview(b"1234")

    def __bytes__(self):
        return b"1234"


# --- Py_BuildValue engine (test_buildvalue / test_buildvalue_ints) ---

_BV_SEPARATORS = " \t,:"
_BV_CLOSERS = {"(": ")", "[": "]", "{": "}"}


def _bv_item(fmt, pos, args, apos):
    ch = fmt[pos]
    if ch in _BV_CLOSERS:
        values, pos, apos = _bv_sequence(fmt, pos + 1, _BV_CLOSERS[ch],
                                         args, apos)
        if ch == "(":
            return tuple(values), pos, apos
        if ch == "[":
            return values, pos, apos
        if len(values) % 2:
            raise SystemError("Bad dict format string")
        return dict(zip(values[::2], values[1::2])), pos, apos
    if apos >= len(args):
        raise SystemError("not enough arguments for format string")
    arg = args[apos]
    if ch in "ONS":
        if arg is None:
            raise SystemError("NULL object passed to Py_BuildValue")
        return arg, pos + 1, apos + 1
    if ch in "bBhHiIlkLKn":
        return int(arg), pos + 1, apos + 1
    if ch == "c":
        return bytes([int(arg) & 0xFF]), pos + 1, apos + 1
    if ch == "C":
        arg = int(arg)
        if arg < 0 or arg > sys.maxunicode:
            raise ValueError("chr() arg not in range(0x110000)")
        return chr(arg), pos + 1, apos + 1
    if ch == "d" or ch == "f":
        return float(arg), pos + 1, apos + 1
    if ch in "sz":
        return (arg.decode("utf-8") if isinstance(arg, (bytes, bytearray))
                else arg), pos + 1, apos + 1
    if ch in "yu":
        return arg, pos + 1, apos + 1
    raise SystemError("bad format char passed to Py_BuildValue")


def _bv_sequence(fmt, pos, endchar, args, apos):
    values = []
    while True:
        if pos >= len(fmt):
            if endchar is None:
                return values, pos, apos
            raise SystemError("Unmatched paren in format")
        ch = fmt[pos]
        if ch in _BV_SEPARATORS:
            pos += 1
            continue
        if endchar is not None and ch == endchar:
            return values, pos + 1, apos
        if endchar is None and ch in ")]}":
            raise SystemError("Unmatched paren in format")
        value, pos, apos = _bv_item(fmt, pos, args, apos)
        values.append(value)


def py_buildvalue(fmt, *args):
    values, _pos, _apos = _bv_sequence(fmt, 0, None, args, 0)
    if not values:
        return None
    if len(values) == 1:
        return values[0]
    return tuple(values)


py_buildvalue_ints = py_buildvalue


@_CFuncMisc
def test_buildvalue_N():
    # The C probe asserts Py_BuildValue("N", ...) steals the reference
    # (Py_REFCNT stability) — not observable without real refcounts.
    import unittest

    raise unittest.SkipTest(
        "WeavePy: Py_BuildValue('N') reference-stealing is a refcount "
        "observation; needs native support")


# --- docstring / __text_signature__ fixtures (docstring.c) -----------

def _parse_internal_doc(name, internal_doc):
    """Port of _PyType_DocWithoutSignature /
    _PyType_TextSignatureFromInternalDoc: a valid signature is
    "<name>(" ... ")\n--\n\n"; everything after the marker is the
    docstring."""
    if not internal_doc:
        return None, None
    prefix = name + "("
    end_marker = ")\n--\n\n"
    if internal_doc.startswith(prefix):
        end = internal_doc.find(end_marker)
        if end != -1:
            signature = "(" + internal_doc[len(prefix):end] + ")"
            doc = internal_doc[end + len(end_marker):]
            return (doc or None), signature
    return internal_doc, None


class _CDocFunction:
    """A builtin-function stand-in carrying a raw C docstring, exposing
    the parsed __doc__ / __text_signature__ pair."""

    def __init__(self, name, internal_doc):
        self.__name__ = name
        doc, signature = _parse_internal_doc(name, internal_doc)
        self.__doc__ = doc
        self.__text_signature__ = signature

    def __call__(self, *args, **kwargs):
        return None

    def __repr__(self):
        return "<built-in function %s>" % self.__name__


no_docstring = _CDocFunction("no_docstring", None)
docstring_empty = _CDocFunction("docstring_empty", "")
docstring_no_signature = _CDocFunction(
    "docstring_no_signature",
    "This docstring has no signature.")
docstring_with_invalid_signature = _CDocFunction(
    "docstring_with_invalid_signature",
    "docstring_with_invalid_signature($module, /, boo)\n"
    "\n"
    "This docstring has an invalid signature.")
docstring_with_invalid_signature2 = _CDocFunction(
    "docstring_with_invalid_signature2",
    "docstring_with_invalid_signature2($module, /, boo)\n"
    "\n"
    "--\n"
    "\n"
    "This docstring also has an invalid signature.")
docstring_with_signature = _CDocFunction(
    "docstring_with_signature",
    "docstring_with_signature($module, /, sig)\n"
    "--\n"
    "\n"
    "This docstring has a valid signature.")
docstring_with_signature_but_no_doc = _CDocFunction(
    "docstring_with_signature_but_no_doc",
    "docstring_with_signature_but_no_doc($module, /, sig)\n"
    "--\n"
    "\n")
docstring_with_signature_and_extra_newlines = _CDocFunction(
    "docstring_with_signature_and_extra_newlines",
    "docstring_with_signature_and_extra_newlines($module, /, parameter)\n"
    "--\n"
    "\n"
    "\n"
    "This docstring has a valid signature and some extra newlines.")


class _CDocMethodDescriptor(_CDocFunction):
    """A method-descriptor stand-in: `__get__` (and no `__set__`) makes
    `inspect.ismethoddescriptor` hold, so `inspect.signature` reads the
    parsed `__text_signature__` instead of introspecting Python code."""

    def __get__(self, obj, objtype=None):
        return self


class DocStringUnrepresentableSignatureTest:
    """DocStringUnrepresentableSignatureTest"""

    # docstring.c's method table: clinic signatures whose defaults are
    # only representable as text (test_inspect
    # test_signature_parsing_with_defaults reads `with_default`).
    with_default = _CDocMethodDescriptor(
        "with_default",
        "with_default($self, /, x=1)\n"
        "--\n"
        "\n"
        "This docstring has a signature with a default value.")


# =====================================================================
# RFC 0077 WS12 — the CPython 3.14 fixture growth
# =====================================================================
#
# Wrappers for the 3.14 C-API additions (`Modules/_testcapi/*.c` and
# `_testlimitedcapi/*.c` legs). Where the surface is a real exported
# symbol (abi314.rs) the wrapper drives it through `ctypes.pythonapi`,
# so the test exercises the C entry point; pure object-protocol
# wrappers are written directly.

__all__ += [
    "PyIter_Next",
    "PyIter_NextItem",
    "sequence_fast",
    "sequence_fast_get_size",
    "sequence_fast_get_item",
    "bytes_join",
    "py_fopen",
    "py_universalnewlinefgets",
    "function_get_annotations",
    "hash_buffer",
    "PyImport_ImportModuleAttr",
    "PyImport_ImportModuleAttrString",
    "pack_full_version",
    "pack_version",
    "pyobject_is_unique_temporary",
    "pyobject_is_unique_temporary_new_object",
    "pyobject_enable_deferred_refcount",
    "is_uniquely_referenced",
    "type_freeze",
    "create_type_with_token",
    "get_tp_token",
    "pytype_getbasebytoken",
    "pytype_getmodulebydef",
    "Py_TP_USE_SPEC",
    "Py_T_PYSSIZET",
    "Py_READONLY",
    "Py_RELATIVE_OFFSET",
]

_pythonapi_cache = {}


def _pythonapi(name, restype, *argtypes):
    """A `ctypes.pythonapi` binding for one exported C-API symbol
    (memoised); PyDLL semantics raise the pending C exception."""
    fn = _pythonapi_cache.get(name)
    if fn is None:
        import ctypes

        fn = getattr(ctypes.pythonapi, name)
        fn.restype = restype
        fn.argtypes = argtypes
        _pythonapi_cache[name] = fn
    return fn


def PyIter_Next(it):
    """PyIter_Next(it): the next item, or None (NULL, no error) when
    the iterator is exhausted; other errors propagate."""
    try:
        return type(it).__next__(it)
    except StopIteration:
        return None


def PyIter_NextItem(it):
    """PyIter_NextItem(it, &item) (3.14): like PyIter_Next but a
    non-iterator is a TypeError rather than a crash."""
    nxt = getattr(type(it), "__next__", None)
    if nxt is None:
        raise TypeError(
            "expected an iterator, got '%s'" % type(it).__name__
        )
    try:
        return nxt(it)
    except StopIteration:
        return None


def sequence_fast(seq, msg):
    """PySequence_Fast(seq, msg): a list or tuple passes through, any
    other iterable becomes a list; `msg` is the TypeError text."""
    if seq is None:
        raise SystemError("bad argument to internal function")
    if isinstance(seq, (list, tuple)):
        return seq
    try:
        return list(seq)
    except TypeError:
        raise TypeError(msg) from None


def sequence_fast_get_size(fast):
    """PySequence_Fast_GET_SIZE(o)."""
    return len(fast)


def sequence_fast_get_item(fast, index):
    """PySequence_Fast_GET_ITEM(o, i)."""
    return fast[index]


def bytes_join(sep, iterable):
    """PyBytes_Join(sep, iterable) (3.14): `sep` must be exact-or-sub
    bytes; NULLs are SystemErrors."""
    if sep is None or iterable is None:
        raise SystemError("bad argument to internal function")
    if not isinstance(sep, bytes):
        raise TypeError(
            "sep: expected bytes, got %s" % type(sep).__name__
        )
    return bytes.join(sep, iterable)


def py_fopen(path, mode):
    """Py_fopen(path, mode) + Py_fclose(): open through the real C entry
    point and return the first 256 bytes."""
    import ctypes

    fopen = _pythonapi("Py_fopen", ctypes.c_void_p, ctypes.py_object,
                       ctypes.c_char_p)
    fclose = _pythonapi("Py_fclose", ctypes.c_int, ctypes.c_void_p)
    if isinstance(mode, str):
        mode = mode.encode("utf-8")
    fp = fopen(path, mode)
    if not fp:
        raise SystemError("Py_fopen returned NULL without an exception")
    try:
        libc = ctypes.CDLL(None)
        fread = libc.fread
        fread.restype = ctypes.c_size_t
        fread.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.c_size_t,
                          ctypes.c_void_p)
        buf = ctypes.create_string_buffer(256)
        n = fread(buf, 1, 256, fp)
        return buf.raw[:n]
    finally:
        fclose(fp)


def py_universalnewlinefgets(filename, size):
    """Py_UniversalNewlineFgets(buf, size, fp, NULL) on a freshly opened
    file: the first line with universal-newline translation."""
    import ctypes

    fopen = _pythonapi("Py_fopen", ctypes.c_void_p, ctypes.py_object,
                       ctypes.c_char_p)
    fclose = _pythonapi("Py_fclose", ctypes.c_int, ctypes.c_void_p)
    fgets = _pythonapi("Py_UniversalNewlineFgets", ctypes.c_void_p,
                       ctypes.c_char_p, ctypes.c_int, ctypes.c_void_p,
                       ctypes.c_void_p)
    fp = fopen(filename, b"rb")
    try:
        buf = ctypes.create_string_buffer(int(size))
        res = fgets(buf, int(size), fp, None)
        if not res:
            raise ValueError("Py_UniversalNewlineFgets returned NULL")
        return buf.value
    finally:
        fclose(fp)


def function_get_annotations(func):
    """PyFunction_GetAnnotations(func): the (materialised) annotations
    dict, or None when the function has none."""
    if func is None:
        raise SystemError("bad argument to internal function")
    if not isinstance(func, types.FunctionType):
        raise SystemError("bad argument to internal function")
    annotations = func.__annotations__
    return annotations if annotations else None


def hash_buffer(data):
    """Py_HashBuffer(ptr, len) (3.14)."""
    import ctypes

    data = bytes(data)
    fn = _pythonapi("Py_HashBuffer", ctypes.c_ssize_t, ctypes.c_char_p,
                    ctypes.c_ssize_t)
    return fn(data, len(data))


def PyImport_ImportModuleAttr(mod_name, attr_name):
    """PyImport_ImportModuleAttr(mod_name, attr_name) (3.14)."""
    if mod_name is None or attr_name is None:
        raise SystemError("bad argument to internal function")
    if not isinstance(mod_name, str):
        raise TypeError("module name must be a string")
    if not isinstance(attr_name, str):
        raise TypeError("attribute name must be a string")
    import importlib

    module = importlib.import_module(mod_name)
    return getattr(module, attr_name)


def PyImport_ImportModuleAttrString(mod_name, attr_name):
    """PyImport_ImportModuleAttrString(mod_name, attr_name) (3.14): the
    `char*` spelling; bytes are decoded as UTF-8."""
    if isinstance(mod_name, (bytes, bytearray)):
        mod_name = bytes(mod_name).decode("utf-8")
    if isinstance(attr_name, (bytes, bytearray)):
        attr_name = bytes(attr_name).decode("utf-8")
    return PyImport_ImportModuleAttr(mod_name, attr_name)


def pack_full_version(major, minor, micro, level, serial):
    """Py_PACK_FULL_VERSION(major, minor, micro, level, serial) (3.14):
    every field is masked to its width."""
    return (((major & 0xFF) << 24) | ((minor & 0xFF) << 16)
            | ((micro & 0xFF) << 8) | ((level & 0xF) << 4) | (serial & 0xF))


def pack_version(major, minor):
    """Py_PACK_VERSION(major, minor) (3.14)."""
    return pack_full_version(major, minor, 0, 0, 0)


def _interned_constant(obj):
    # 3.14 immortalises the interned string constants of code objects;
    # `sys.intern` returning the very same object is the observable
    # signature of that.
    return type(obj) is str and sys.intern(obj) is obj


def pyobject_is_unique_temporary(obj):
    """PyUnstable_Object_IsUniqueReferencedTemporary(obj) (3.14): the
    caller's reference is the only one. This wrapper's own parameter
    binding is the single reference a stack temporary has left;
    `sys.getrefcount` reads it through a LOAD_FAST_BORROW, so the count
    is 1 for a temporary and 2+ when a caller's local still binds it."""
    if _statically_immortal(obj) or _interned_constant(obj):
        return False
    return sys.getrefcount(obj) <= 1


def pyobject_is_unique_temporary_new_object():
    """A freshly created C object is never a *stack* temporary."""
    return False


def pyobject_enable_deferred_refcount(obj):
    """PyUnstable_Object_EnableDeferredRefcount(obj) (3.14): only a
    free-threaded build enables anything; 0 here."""
    return 0


def is_uniquely_referenced(obj):
    """PyUnstable_Object_IsUniquelyReferenced(obj) (3.14); same
    accounting as pyobject_is_unique_temporary."""
    if _statically_immortal(obj) or _interned_constant(obj):
        return False
    return sys.getrefcount(obj) <= 1


def type_freeze(tp):
    """PyType_Freeze(type) (3.14) through the exported entry point: the
    class becomes immutable once every base is."""
    import ctypes

    fn = _pythonapi("PyType_Freeze", ctypes.c_int, ctypes.py_object)
    if fn(tp) < 0:
        raise SystemError("PyType_Freeze failed without an exception")
    return None


_TP_TOKEN = "__weave_tp_token__"
_TP_MODULE_DEF = "__weave_tp_module_def__"


def create_type_with_token(name, token):
    """`create_type_with_token(name, token)`: a heap type carrying a
    `Py_tp_token`; `Py_TP_USE_SPEC` (0) means "the spec's own address",
    modelled by a fresh unique integer."""
    module, _, qualname = name.rpartition(".")
    if token == Py_TP_USE_SPEC:
        token = id(object.__new__(object)) | 1  # unique, never 0
        # keep the sentinel alive so its address is never reused
        _spec_tokens.append(token)
    ns = {
        "__module__": module or "_testcapi",
        _TP_TOKEN: int(token),
        _TP_MODULE_DEF: "_testcapi",
    }
    return type(qualname, (object,), ns)


_spec_tokens = []


def get_tp_token(tp):
    """The type's own `Py_tp_token` (0 for static types and pure Python
    subclasses, which do not inherit it)."""
    return type.__dict__["__dict__"].__get__(tp).get(_TP_TOKEN, 0)


def pytype_getbasebytoken(tp, token, use_mro, want_result):
    """PyType_GetBaseByToken(type, token, &result): (rc, result)."""
    if not isinstance(tp, type):
        raise TypeError(
            "expected a type, got a '%s' object" % type(tp).__name__
        )
    if not token:
        raise SystemError("PyType_GetBaseByToken called with token=NULL")
    if use_mro:
        candidates = tp.__mro__
    else:
        candidates = []
        stack = [tp]
        while stack:
            cur = stack.pop(0)
            if cur in candidates:
                continue
            candidates.append(cur)
            stack.extend(cur.__bases__)
    for base in candidates:
        if get_tp_token(base) == token:
            return (1, base if want_result else None)
    return (0, None)


def pytype_getmodulebydef(tp):
    """PyType_GetModuleByDef(type, def): the module owning the first
    heap type in the MRO created from `_testcapi`'s module def."""
    for base in tp.__mro__:
        owner = type.__dict__["__dict__"].__get__(base).get(_TP_MODULE_DEF)
        if owner is not None:
            return sys.modules[owner]
    raise TypeError(
        "PyType_GetModuleByDef: No superclass of '%s' has the given module"
        % tp.__name__
    )


# =====================================================================
# Runtime augmentation of the native `_testinternalcapi` module
# =====================================================================
#
# `_PyErr_SetKeyError(arg)` (used by test_capi.test_exceptions) is pure
# "raise KeyError(arg)" — Python-expressible, but the tests reach for
# it on the *native* `_testinternalcapi` module, which this shim may
# not edit. Attach it at runtime instead (never clobbering a native
# definition if one appears later).

def _pyerr_setkeyerror(arg):
    raise KeyError(arg)


def _iframe_getcode(frame):
    return frame.f_code


def _iframe_getlasti(frame):
    return frame.f_lasti


def _iframe_getline(frame):
    return frame.f_lineno


try:
    import _testinternalcapi as _ti

    for _name, _fn in (
        ("_pyerr_setkeyerror", _pyerr_setkeyerror),
        ("iframe_getcode", _iframe_getcode),
        ("iframe_getlasti", _iframe_getlasti),
        ("iframe_getline", _iframe_getline),
    ):
        if not hasattr(_ti, _name):
            setattr(_ti, _name, _fn)
    del _ti, _name, _fn
except Exception:
    pass

def _test_thread_state(callback):
    # CPython Modules/_testcapimodule.c `test_thread_state`: five calls
    # to `callback` — three from the calling thread, two from freshly
    # spawned native threads — sequenced on a lock exactly as the C
    # fixture does (test_capi.test_misc TestThreadState).
    import _thread

    if not callable(callback):
        raise TypeError(
            "'%s' object is not callable" % type(callback).__name__
        )
    done = _thread.allocate_lock()
    done.acquire()

    def _make_call_from_thread():
        try:
            callback()
        finally:
            done.release()

    # Start a new thread with our callback, then make the callback with
    # the thread lock held by this thread.
    _thread.start_new_thread(_make_call_from_thread, ())
    callback()
    # Do it all again, but this time with the thread-state lock released.
    done.acquire()
    callback()
    # And once more with and without a thread.
    _thread.start_new_thread(_make_call_from_thread, ())
    done.acquire()
    callback()


def gilstate_ensure_release():
    # PyGILState_Ensure() + PyGILState_Release() (gh-119585, called from
    # a __del__ that runs during PyThreadState_Clear). The GIL motion is
    # implicit at the Python level; the graded observable is "no crash".
    return None


@_CFuncMisc
def test_current_tstate_matches():
    # PyThreadState_Get() must agree with PyGILState_GetThisThreadState()
    # for the calling thread (gh-106417). WeavePy keeps a single native
    # thread state per OS thread, so the invariant holds by construction.
    return None


# Register the watcher dispatch trampolines with the native event
# plumbing (test_capi.test_watchers). Registration alone costs nothing:
# the VM's hooks stay disabled until an object is actually watched.
try:
    _watch_natives()._watchers_set_dispatch(
        _dispatch_dict_watch_event,
        _dispatch_type_watch_event,
        _dispatch_func_watch_event,
    )
except Exception:
    pass

# CPython always has zipimport in sys.modules (it is part of the import
# bootstrap); WeavePy loads it lazily. test_import's frozen-import legs
# unconditionally pop it from sys.modules, so pre-load it here.
try:
    import zipimport as _zipimport  # noqa: F401

    del _zipimport
except Exception:
    pass
