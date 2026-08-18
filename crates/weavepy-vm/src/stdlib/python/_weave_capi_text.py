"""RFC 0068 WS3 — unicode/codecs/eval/file/sys C-API fixture shims.

Python ports of the `Modules/_testlimitedcapi/{unicode,codec,eval,
file,sys}.c` (and matching `Modules/_testcapi/*.c`) fixture wrappers,
star-imported by the frozen `_testcapi` / `_testlimitedcapi` shims.
Every exported name must be listed in `__all__`.

Conventions shared with the C fixtures: the CPython test files set
`NULL = None`, and the wrappers run each argument through `NULLABLE()`
(Py_None -> C NULL) — so `None` below always plays the role of a NULL
pointer, never of the Python object `None`.
"""

import codecs as _codecs
import sys as _sys
import types as _types

__all__ = [
    # test_capi.test_codecs / test_capi.test_unicode
    "unicode_fromencodedobject",
    "unicode_decode",
    "unicode_asencodedstring",
    "unicode_decodeutf8",
    "unicode_decodeutf8stateful",
    "unicode_asutf8",
    "unicode_asutf8string",
    "unicode_decodeutf16",
    "unicode_decodeutf16stateful",
    "unicode_asutf16string",
    "unicode_decodeutf32",
    "unicode_decodeutf32stateful",
    "unicode_asutf32string",
    "unicode_decodelatin1",
    "unicode_aslatin1string",
    "unicode_decodeascii",
    "unicode_asasciistring",
    "unicode_decodecharmap",
    "unicode_ascharmapstring",
    "unicode_decodeunicodeescape",
    "unicode_asunicodeescapestring",
    "unicode_decoderawunicodeescape",
    "unicode_asrawunicodeescapestring",
    # test_capi.test_eval
    "eval_get_func_name",
    "eval_get_func_desc",
    "eval_getlocals",
    "eval_getglobals",
    "eval_getbuiltins",
    "eval_getframe",
    "eval_getframe_builtins",
    "eval_getframe_globals",
    "eval_getframe_locals",
    "eval_get_recursion_limit",
    "eval_set_recursion_limit",
    # test_capi.test_eval_code_ex
    "eval_code_ex",
    # test_capi.test_file
    "pyfile_fromfd",
    "pyfile_getline",
    "pyfile_writestring",
    "pyfile_writeobject",
    "pyobject_asfiledescriptor",
    "pyfile_newstdprinter",
    # test_capi.test_sys
    "sys_getobject",
    "sys_setobject",
    "sys_getxoptions",
]


def _bad_internal_call():
    # PyErr_BadInternalCall()
    return SystemError("bad argument to internal function")


def _bad_argument():
    # PyErr_BadArgument()
    return TypeError("bad argument type for built-in operation")


def _as_bytes_buffer(data):
    # PyArg_ParseTuple "y#": a C contiguous read-only byte buffer.
    if isinstance(data, bytes):
        return data
    if isinstance(data, (bytearray, memoryview)):
        return bytes(data)
    raise TypeError(
        "a bytes-like object is required, not %r" % type(data).__name__
    )


def _check_str(obj):
    # PyUnicode_Check() guard used by the PyUnicode_As*String() family.
    if not isinstance(obj, str):
        raise _bad_argument()


# ---------------------------------------------------------------------------
# test_capi.test_codecs — PyUnicode_{Decode,AsEncodedString,...} wrappers
# (Modules/_testlimitedcapi/unicode.c)
# ---------------------------------------------------------------------------


def unicode_fromencodedobject(obj, encoding, errors=None):
    """PyUnicode_FromEncodedObject(obj, encoding, errors)."""
    if obj is None:
        raise _bad_internal_call()
    if isinstance(obj, str):
        raise TypeError("decoding str is not supported")
    if not isinstance(obj, (bytes, bytearray, memoryview)):
        raise TypeError(
            "decoding to str: need a bytes-like object, %s found"
            % type(obj).__name__
        )
    return bytes(obj).decode(encoding or "utf-8", errors or "strict")


def unicode_decode(data, encoding, errors=None):
    """PyUnicode_Decode(s, size, encoding, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode(encoding or "utf-8", errors or "strict")


def unicode_asencodedstring(unicode, encoding, errors=None):
    """PyUnicode_AsEncodedString(unicode, encoding, errors)."""
    _check_str(unicode)
    return unicode.encode(encoding or "utf-8", errors or "strict")


def unicode_decodeutf8(data, errors=None):
    """PyUnicode_DecodeUTF8(data, size, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode("utf-8", errors or "strict")


def unicode_decodeutf8stateful(data, errors=None):
    """PyUnicode_DecodeUTF8Stateful(data, size, errors, &consumed)."""
    data = _as_bytes_buffer(data)
    return _codecs.utf_8_decode(data, errors or "strict", False)


def unicode_asutf8(unicode, buflen):
    """PyUnicode_AsUTF8(unicode) + PyBytes_FromStringAndSize(s, buflen).

    The C fixture reads `buflen` bytes out of the cached UTF-8 buffer,
    which the tests size to cover the NUL terminator.
    """
    _check_str(unicode)
    buf = unicode.encode("utf-8") + b"\0"
    if buflen <= len(buf):
        return buf[:buflen]
    return buf + b"\0" * (buflen - len(buf))


def unicode_asutf8string(unicode):
    """PyUnicode_AsUTF8String(unicode)."""
    _check_str(unicode)
    return unicode.encode("utf-8")


def unicode_decodeutf16(byteorder, data, errors=None):
    """PyUnicode_DecodeUTF16(data, size, errors, &byteorder)."""
    data = _as_bytes_buffer(data)
    res, _consumed, bo = _codecs.utf_16_ex_decode(
        data, errors or "strict", byteorder, True
    )
    return (bo, res)


def unicode_decodeutf16stateful(byteorder, data, errors=None):
    """PyUnicode_DecodeUTF16Stateful(data, size, errors, &byteorder, &consumed)."""
    data = _as_bytes_buffer(data)
    res, consumed, bo = _codecs.utf_16_ex_decode(
        data, errors or "strict", byteorder, False
    )
    return (bo, res, consumed)


def unicode_asutf16string(unicode):
    """PyUnicode_AsUTF16String(unicode)."""
    _check_str(unicode)
    return unicode.encode("utf-16")


def unicode_decodeutf32(byteorder, data, errors=None):
    """PyUnicode_DecodeUTF32(data, size, errors, &byteorder)."""
    data = _as_bytes_buffer(data)
    res, _consumed, bo = _codecs.utf_32_ex_decode(
        data, errors or "strict", byteorder, True
    )
    return (bo, res)


def unicode_decodeutf32stateful(byteorder, data, errors=None):
    """PyUnicode_DecodeUTF32Stateful(data, size, errors, &byteorder, &consumed)."""
    data = _as_bytes_buffer(data)
    res, consumed, bo = _codecs.utf_32_ex_decode(
        data, errors or "strict", byteorder, False
    )
    return (bo, res, consumed)


def unicode_asutf32string(unicode):
    """PyUnicode_AsUTF32String(unicode)."""
    _check_str(unicode)
    return unicode.encode("utf-32")


def unicode_decodelatin1(data, errors=None):
    """PyUnicode_DecodeLatin1(data, size, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode("latin-1", errors or "strict")


def unicode_aslatin1string(unicode):
    """PyUnicode_AsLatin1String(unicode)."""
    _check_str(unicode)
    return unicode.encode("latin-1")


def unicode_decodeascii(data, errors=None):
    """PyUnicode_DecodeASCII(data, size, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode("ascii", errors or "strict")


def unicode_asasciistring(unicode):
    """PyUnicode_AsASCIIString(unicode)."""
    _check_str(unicode)
    return unicode.encode("ascii")


def unicode_decodecharmap(data, mapping, errors=None):
    """PyUnicode_DecodeCharmap(data, size, mapping, errors).

    A NULL mapping decodes as Latin-1 (which charmap_decode also does
    for a None mapping).
    """
    data = _as_bytes_buffer(data)
    return _codecs.charmap_decode(data, errors or "strict", mapping)[0]


def unicode_ascharmapstring(unicode, mapping):
    """PyUnicode_AsCharmapString(unicode, mapping)."""
    if unicode is None or mapping is None or not isinstance(unicode, str):
        # PyErr_BadArgument() — the C function rejects a NULL mapping and
        # a non-str unicode outright.
        raise _bad_argument()
    return _codecs.charmap_encode(unicode, "strict", mapping)[0]


def unicode_decodeunicodeescape(data, errors=None):
    """PyUnicode_DecodeUnicodeEscape(data, size, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode("unicode_escape", errors or "strict")


def unicode_asunicodeescapestring(unicode):
    """PyUnicode_AsUnicodeEscapeString(unicode)."""
    _check_str(unicode)
    return unicode.encode("unicode_escape")


def unicode_decoderawunicodeescape(data, errors=None):
    """PyUnicode_DecodeRawUnicodeEscape(data, size, errors)."""
    data = _as_bytes_buffer(data)
    return data.decode("raw_unicode_escape", errors or "strict")


def unicode_asrawunicodeescapestring(unicode):
    """PyUnicode_AsRawUnicodeEscapeString(unicode)."""
    _check_str(unicode)
    return unicode.encode("raw_unicode_escape")


# ---------------------------------------------------------------------------
# test_capi.test_eval — PyEval_* wrappers (Modules/_testlimitedcapi/eval.c).
# Every fixture peels the *caller's* frame with sys._getframe(1), exactly
# like the C wrappers observing the frame of the code that called them.
# ---------------------------------------------------------------------------


def eval_get_func_name(func):
    """PyEval_GetFuncName(func)."""
    if isinstance(func, _types.MethodType):
        return eval_get_func_name(func.__func__)
    if isinstance(func, (_types.FunctionType, _types.BuiltinFunctionType)):
        return func.__name__
    return type(func).__name__


def eval_get_func_desc(func):
    """PyEval_GetFuncDesc(func)."""
    if isinstance(
        func,
        (_types.MethodType, _types.FunctionType, _types.BuiltinFunctionType),
    ):
        return "()"
    return " object"


def eval_getlocals():
    """PyEval_GetLocals()."""
    return _sys._getframe(1).f_locals


def eval_getglobals():
    """PyEval_GetGlobals()."""
    return _sys._getframe(1).f_globals


def eval_getbuiltins():
    """PyEval_GetBuiltins()."""
    return _sys._getframe(1).f_builtins


def eval_getframe():
    """PyEval_GetFrame()."""
    return _sys._getframe(1)


def eval_getframe_builtins():
    """PyEval_GetFrameBuiltins()."""
    return _sys._getframe(1).f_builtins


def eval_getframe_globals():
    """PyEval_GetFrameGlobals()."""
    return _sys._getframe(1).f_globals


def eval_getframe_locals():
    """PyEval_GetFrameLocals() — returns a fresh dict (PEP 667)."""
    return dict(_sys._getframe(1).f_locals)


def eval_get_recursion_limit():
    """Py_GetRecursionLimit()."""
    return _sys.getrecursionlimit()


def eval_set_recursion_limit(limit):
    """Py_SetRecursionLimit(limit)."""
    _sys.setrecursionlimit(limit)


# ---------------------------------------------------------------------------
# test_capi.test_eval_code_ex — PyEval_EvalCodeEx() (Modules/_testcapimodule.c
# eval_eval_code_ex). Parse format "OO|OO!O!O!OO" with NULLABLE code/globals/
# locals/kw_defaults/closure.
# ---------------------------------------------------------------------------


def eval_code_ex(
    code,
    globals,
    locals=None,
    args=None,
    kwargs=None,
    defaults=None,
    kw_defaults=None,
    closure=None,
):
    """PyEval_EvalCodeEx(code, globals, locals, args, kwargs, defs, kwdefs,
    closure)."""
    # PyArg_ParseTuple "O!" checks (TypeError before anything runs).
    if args is not None and not isinstance(args, tuple):
        raise TypeError(
            "eval_code_ex() argument 4 must be tuple, not %s"
            % type(args).__name__
        )
    if kwargs is not None and not isinstance(kwargs, dict):
        raise TypeError(
            "eval_code_ex() argument 5 must be dict, not %s"
            % type(kwargs).__name__
        )
    if defaults is not None and not isinstance(defaults, tuple):
        raise TypeError(
            "eval_code_ex() argument 6 must be tuple, not %s"
            % type(defaults).__name__
        )
    # _PyEval_BuiltinsFromGlobals -> PyDict_GetItemWithError on a non-dict
    # globals -> PyErr_BadInternalCall() (SystemError). PyDict_Check, so
    # dict subclasses pass but UserDict / list / int do not.
    if not isinstance(globals, dict):
        raise _bad_internal_call()

    if args is None:
        args = ()
    if kwargs is None:
        kwargs = {}

    if code.co_flags & 0x01:  # CO_OPTIMIZED: function-style code object.
        func = _types.FunctionType(
            code, globals, None, defaults if defaults else None, closure
        )
        if kw_defaults is not None:
            if isinstance(kw_defaults, dict):
                func.__kwdefaults__ = kw_defaults
            else:
                # The kw-defaults lookup is PyDict_GetItemWithError(kwdefs,
                # name) — it only runs (and only then raises SystemError for
                # a non-dict) when a keyword-only argument is actually
                # missing from kwargs.
                kwonly = code.co_varnames[
                    code.co_argcount : code.co_argcount + code.co_kwonlyargcount
                ]
                if any(name not in kwargs for name in kwonly):
                    raise _bad_internal_call()
        return func(*args, **kwargs)

    # Class-body / module-style code: evaluate against the explicit locals
    # mapping (NULL locals falls back to globals).
    if locals is None:
        locals = globals
    exec(code, globals, locals)
    return None


# ---------------------------------------------------------------------------
# test_capi.test_file — PyFile_* / PyObject_AsFileDescriptor wrappers
# (Modules/_testlimitedcapi/file.c and Modules/_testcapi/file.c).
# ---------------------------------------------------------------------------


def pyfile_fromfd(fd, name, mode, buffering, encoding, errors, newline, closefd):
    """PyFile_FromFd() — a thin wrapper over _io.open(); `name` is ignored
    just like the C function."""
    import _io

    return _io.open(fd, mode, buffering, encoding, errors, newline, bool(closefd))


def pyfile_getline(file, n):
    """PyFile_GetLine(file, n)."""
    if n <= 0:
        result = file.readline()
    else:
        result = file.readline(n)
    if not isinstance(result, (bytes, str)):
        raise TypeError("object.readline() returned non-string")
    if n < 0:
        if not result:
            raise EOFError("EOF when reading a line")
        newline = b"\n" if isinstance(result, bytes) else "\n"
        if result.endswith(newline):
            result = result[: -1]
    return result


def pyfile_writestring(data, file):
    """PyFile_WriteString(s, file) — `s` arrives as the C UTF-8 buffer."""
    if file is None:
        raise SystemError("null file for PyFile_WriteString")
    if isinstance(data, str):
        data = data.encode("utf-8")
    text = _as_bytes_buffer(data).decode("utf-8")
    file.write(text)
    return 0


def pyfile_writeobject(obj, file, flags):
    """PyFile_WriteObject(obj, file, flags)."""
    if file is None:
        raise TypeError("writeobject with NULL file")
    writer = file.write  # AttributeError for objects without .write
    if obj is None:
        value = "<NULL>"
    elif flags & 1:  # Py_PRINT_RAW
        value = str(obj)
    else:
        value = repr(obj)
    writer(value)
    return 0


def pyobject_asfiledescriptor(obj):
    """PyObject_AsFileDescriptor(obj)."""
    if isinstance(obj, int):
        if isinstance(obj, bool):
            import warnings

            warnings.warn(
                "bool is used as a file descriptor", RuntimeWarning, stacklevel=2
            )
        fd = int(obj)
    else:
        fileno = getattr(obj, "fileno", None)
        if fileno is not None:
            fd = fileno()
            if not isinstance(fd, int):
                raise TypeError("fileno() returned a non-integer")
            fd = int(fd)
        else:
            raise TypeError(
                "argument must be an int, or have a fileno() method."
            )
    if fd < 0:
        raise ValueError(
            "file descriptor cannot be a negative integer (%d)" % fd
        )
    return fd


class _StdPrinter:
    """PyStdPrinter_Type emulation (Objects/fileobject.c).

    Instances are minted only through pyfile_newstdprinter() via
    object.__new__; calling the type itself raises like a type whose
    tp_new is disallowed (support.check_disallow_instantiation).
    """

    closed = False
    encoding = None
    mode = "w"

    def __new__(cls, *args, **kwargs):
        mod = cls.__module__
        name = cls.__name__
        qualname = name if mod == "builtins" else "%s.%s" % (mod, name)
        raise TypeError("cannot create '%s' instances" % qualname)

    def fileno(self):
        return self._fd

    def isatty(self):
        import os

        return os.isatty(self._fd)

    def write(self, text):
        # stdprinter_write: UTF-8 with backslashreplace, raw _Py_write();
        # returns the byte count written.
        import os

        data = text.encode("utf-8", "backslashreplace")
        return os.write(self._fd, data)

    def flush(self):
        return None

    def close(self):
        return None


def pyfile_newstdprinter(fd):
    """PyFile_NewStdPrinter(fd)."""
    printer = object.__new__(_StdPrinter)
    printer._fd = fd
    return printer


# ---------------------------------------------------------------------------
# test_capi.test_sys — PySys_{Get,Set}Object / PySys_GetXOptions wrappers
# (Modules/_testlimitedcapi/sys.c).
# ---------------------------------------------------------------------------


class _UnraisableArgs:
    """Duck-typed stand-in for sys.unraisablehook's UnraisableHookArgs."""

    def __init__(self, exc, err_msg):
        self.exc_type = type(exc)
        self.exc_value = exc
        self.exc_traceback = exc.__traceback__
        self.err_msg = err_msg
        self.object = None


def _decode_name(name):
    # PyArg_Parse "z#" hands the C fixture a raw byte buffer.
    if isinstance(name, str):
        return name
    return _as_bytes_buffer(name).decode("utf-8")


def sys_getobject(name):
    """PySys_GetObject(name) — NULL result maps to the AttributeError
    sentinel; a lookup error is swallowed and reported as unraisable."""
    try:
        attr = _decode_name(name)
    except UnicodeDecodeError as exc:
        _sys.unraisablehook(
            _UnraisableArgs(exc, "Exception ignored in PySys_GetObject()")
        )
        return AttributeError
    return getattr(_sys, attr, AttributeError)


def sys_setobject(name, value):
    """PySys_SetObject(name, value) — a NULL value deletes; deleting a
    missing attribute still succeeds."""
    attr = _decode_name(name)  # UnicodeDecodeError propagates
    if value is None:
        try:
            delattr(_sys, attr)
        except AttributeError:
            pass
        return 0
    setattr(_sys, attr, value)
    return 0


def sys_getxoptions():
    """PySys_GetXOptions() — resets sys._xoptions to a fresh dict when it
    is missing or not a dict."""
    xoptions = getattr(_sys, "_xoptions", None)
    if not isinstance(xoptions, dict):
        xoptions = {}
        _sys._xoptions = xoptions
    return xoptions
