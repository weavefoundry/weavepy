"""RFC 0068 WS3 — bytes/bytearray/object/abstract C-API fixture shims.

Python ports of the `Modules/_testlimitedcapi/{bytearray,bytes,object,
abstract}.c` (and matching `Modules/_testcapi/*.c`) fixture wrappers,
star-imported by the frozen `_testcapi` / `_testlimitedcapi` shims.
Every exported name must be listed in `__all__`.

Convention: like the C fixtures' `NULLABLE(x)`, a Python `None`
argument stands for C `NULL`. Wrappers reproduce the observable
behavior of the underlying C API calls: return values, exception
types, and the exact messages the tests assert.
"""

import sys as _sys

__all__ = [
    # Modules/_testlimitedcapi/bytearray.c
    "bytearray_check",
    "bytearray_checkexact",
    "bytearray_fromstringandsize",
    "bytearray_fromobject",
    "bytearray_size",
    "bytearray_asstring",
    "bytearray_concat",
    "bytearray_resize",
    # Modules/_testlimitedcapi/bytes.c
    "bytes_check",
    "bytes_checkexact",
    "bytes_fromstringandsize",
    "bytes_fromstring",
    "bytes_fromobject",
    "bytes_size",
    "bytes_asstring",
    "bytes_asstringandsize",
    "bytes_asstringandsize_null",
    "bytes_repr",
    "bytes_concat",
    "bytes_concatanddel",
    "bytes_decodeescape",
    # Modules/_testcapi/bytes.c
    "bytes_resize",
    # Modules/_testlimitedcapi/object.c
    "get_constant",
    "get_constant_borrowed",
    # Modules/_testcapi/object.c
    "call_pyobject_print",
    "pyobject_print_null",
    "pyobject_print_noref_object",
    "pyobject_print_os_error",
    "pyobject_clear_weakrefs_no_callbacks",
    # Modules/_testcapi/hash.c (used by test_capi.test_abstract only)
    "object_generichash",
    # Modules/_testlimitedcapi/abstract.c — PyObject_* object protocol
    "object_str",
    "object_repr",
    "object_ascii",
    "object_bytes",
    "object_getattr",
    "object_getattrstring",
    "object_hasattr",
    "object_setattr",
    "object_setattrstring",
    "object_delattr",
    "object_delattrstring",
    # Modules/_testcapi/abstract.c — optional-attr probes
    "object_getoptionalattr",
    "object_getoptionalattrstring",
    "object_hasattrwitherror",
    "object_hasattrstringwitherror",
    # Modules/_testlimitedcapi/abstract.c — PyMapping_*
    "mapping_check",
    "mapping_size",
    "mapping_length",
    "object_getitem",
    "mapping_getitemstring",
    "mapping_haskey",
    "mapping_haskeystring",
    "mapping_haskeywitherror",
    "mapping_haskeystringwitherror",
    "object_setitem",
    "mapping_setitemstring",
    "object_delitem",
    "mapping_delitem",
    "mapping_delitemstring",
    "mapping_keys",
    "mapping_values",
    "mapping_items",
    # Modules/_testcapi/abstract.c — optional-item probes
    "mapping_getoptionalitem",
    "mapping_getoptionalitemstring",
    # Modules/_testlimitedcapi/abstract.c — PySequence_*
    # (sequence_getitem/setitem/delitem intentionally NOT defined:
    # earlier fixtures with those names already exist in the
    # _testlimitedcapi shim and must not be shadowed.)
    "sequence_check",
    "sequence_size",
    "sequence_length",
    "sequence_concat",
    "sequence_repeat",
    "sequence_inplaceconcat",
    "sequence_inplacerepeat",
    "sequence_setslice",
    "sequence_delslice",
    "sequence_count",
    "sequence_contains",
    "sequence_index",
    "sequence_list",
    "sequence_tuple",
]


# Any single allocation this large cannot succeed; the C allocators
# fail and the API surfaces MemoryError (or OverflowError for the
# _PyBytesWriter overallocation check).
_ALLOC_LIMIT = 1 << 48


def _null_error():
    # abstract.c null_error()
    raise SystemError("null argument to internal routine")


def _bad_internal_call():
    # PyErr_BadInternalCall()
    raise SystemError("bad argument to internal function")


def _check_attr_name(name):
    # PyObject_GetAttr & friends reject non-str names up front.
    if not isinstance(name, str):
        raise TypeError(
            "attribute name must be string, not '%s'" % type(name).__name__
        )


def _decode_name(name):
    # A C `const char *` arrives as bytes; PyUnicode_FromString()
    # decodes it as UTF-8 (raising UnicodeDecodeError).
    if isinstance(name, (bytes, bytearray)):
        return bytes(name).decode("utf-8")
    return name


class _UnraisableHookArgs:
    """Shape-compatible stand-in for sys.unraisablehook's argument."""

    def __init__(self, exc, err_msg, obj):
        self.exc_type = type(exc)
        self.exc_value = exc
        self.exc_traceback = getattr(exc, "__traceback__", None)
        self.err_msg = err_msg
        self.object = obj


def _write_unraisable(exc, err_msg=None, obj=None):
    # PyErr_FormatUnraisable(): route the swallowed exception through
    # sys.unraisablehook so test.support.catch_unraisable_exception
    # observes it.
    hook = getattr(_sys, "unraisablehook", None)
    if hook is None:
        return
    try:
        hook(_UnraisableHookArgs(exc, err_msg, obj))
    except BaseException:
        pass


def _as_simple_bytes(obj):
    # PyObject_GetBuffer(obj, PyBUF_SIMPLE): a C-contiguous byte view.
    if isinstance(obj, bytes):
        return obj
    if isinstance(obj, bytearray):
        return bytes(obj)
    m = obj if isinstance(obj, memoryview) else memoryview(obj)
    if not m.c_contiguous:
        raise BufferError("underlying buffer is not C-contiguous")
    return m.tobytes()


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/bytearray.c


def bytearray_check(obj):
    # PyByteArray_Check()
    return 1 if isinstance(obj, bytearray) else 0


def bytearray_checkexact(obj):
    # PyByteArray_CheckExact()
    return 1 if type(obj) is bytearray else 0


def bytearray_fromstringandsize(s, size=None):
    # PyByteArray_FromStringAndSize()
    if size is None:
        size = 0 if s is None else len(s)
    if size < 0:
        raise SystemError(
            "Negative size passed to PyByteArray_FromStringAndSize"
        )
    if size > _ALLOC_LIMIT:
        raise MemoryError
    if s is None:
        # NULL source: the C API returns an uninitialized buffer.
        return bytearray(size)
    s = bytes(s)
    if size <= len(s):
        return bytearray(s[:size])
    return bytearray(s + b"\x00" * (size - len(s)))


def bytearray_fromobject(arg):
    # PyByteArray_FromObject() == bytearray(arg)
    return bytearray(arg)


def bytearray_size(arg):
    # PyByteArray_Size(); only ever called with a real bytearray
    # (anything else is a documented crash in the C fixture).
    return len(arg)


def bytearray_asstring(obj, buflen):
    # PyByteArray_AsString() + PyByteArray_FromStringAndSize(s, buflen):
    # the internal buffer always carries a trailing NUL byte.
    data = bytes(obj) + b"\x00"
    if buflen > len(data):
        data = data + b"\x00" * (buflen - len(data))
    return bytearray(data[:buflen])


def bytearray_concat(left, right):
    # PyByteArray_Concat(): both sides via PyBUF_SIMPLE buffers.
    try:
        va = _as_simple_bytes(left)
        vb = _as_simple_bytes(right)
    except (TypeError, BufferError):
        raise TypeError(
            "can't concat %s to %s"
            % (type(right).__name__, type(left).__name__)
        ) from None
    return bytearray(va + vb)


def bytearray_resize(obj, size):
    # PyByteArray_Resize(); resizes in place, returns 0.
    if size > _ALLOC_LIMIT:
        raise MemoryError
    cur = len(obj)
    if size < cur:
        del obj[size:]
    elif size > cur:
        obj.extend(b"\x00" * (size - cur))
    return 0


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/bytes.c


def bytes_check(obj):
    # PyBytes_Check()
    return 1 if isinstance(obj, bytes) else 0


def bytes_checkexact(obj):
    # PyBytes_CheckExact()
    return 1 if type(obj) is bytes else 0


def bytes_fromstringandsize(s, size=None):
    # PyBytes_FromStringAndSize()
    if size is None:
        size = 0 if s is None else len(s)
    if size < 0:
        raise SystemError("Negative size passed to PyBytes_FromStringAndSize")
    if size > _ALLOC_LIMIT:
        raise MemoryError
    if s is None:
        return b"\x00" * size
    s = bytes(s)
    if size <= len(s):
        return s[:size]
    return s + b"\x00" * (size - len(s))


def bytes_fromstring(arg):
    # PyBytes_FromString(): reads a C string up to the first NUL.
    data = bytes(arg)
    cut = data.find(0)
    return data[:cut] if cut >= 0 else data


def _pybytes_fromobject(x):
    # PyBytes_FromObject()
    if x is None:
        _bad_internal_call()
    if type(x) is bytes:
        return x
    if isinstance(x, str):
        raise TypeError("cannot convert 'str' object to bytes")
    if isinstance(x, (bytes, bytearray, memoryview)):
        return bytes(x)
    try:
        it = iter(x)
    except TypeError:
        raise TypeError(
            "cannot convert '%s' object to bytes" % type(x).__name__
        ) from None
    out = bytearray()
    for item in it:
        out.append(item)
    return bytes(out)


def bytes_fromobject(arg):
    return _pybytes_fromobject(arg)


def bytes_size(arg):
    # PyBytes_Size()
    if not isinstance(arg, bytes):
        raise TypeError("expected bytes, %s found" % type(arg).__name__)
    return len(arg)


def bytes_asstring(obj, buflen):
    # PyBytes_AsString() + PyBytes_FromStringAndSize(s, buflen)
    if not isinstance(obj, bytes):
        raise TypeError("expected bytes, %s found" % type(obj).__name__)
    data = bytes(obj) + b"\x00"
    if buflen > len(data):
        data = data + b"\x00" * (buflen - len(data))
    return data[:buflen]


def bytes_asstringandsize(obj, buflen):
    # PyBytes_AsStringAndSize(obj, &s, &size)
    if not isinstance(obj, bytes):
        raise TypeError("expected bytes, %s found" % type(obj).__name__)
    data = bytes(obj) + b"\x00"
    if buflen > len(data):
        data = data + b"\x00" * (buflen - len(data))
    return (data[:buflen], len(obj))


def bytes_asstringandsize_null(obj, buflen):
    # PyBytes_AsStringAndSize(obj, &s, NULL): rejects embedded NULs.
    if not isinstance(obj, bytes):
        raise TypeError("expected bytes, %s found" % type(obj).__name__)
    if 0 in obj:
        raise ValueError("embedded null byte")
    data = bytes(obj) + b"\x00"
    if buflen > len(data):
        data = data + b"\x00" * (buflen - len(data))
    return data[:buflen]


def bytes_repr(obj, smartquotes):
    # PyBytes_Repr()
    data = bytes(obj)
    quote = 0x27  # '
    if smartquotes and 0x27 in data and 0x22 not in data:
        quote = 0x22  # "
    parts = ["b", chr(quote)]
    for c in data:
        if c == quote or c == 0x5C:
            parts.append("\\" + chr(c))
        elif c == 0x09:
            parts.append("\\t")
        elif c == 0x0A:
            parts.append("\\n")
        elif c == 0x0D:
            parts.append("\\r")
        elif c < 0x20 or c >= 0x7F:
            parts.append("\\x%02x" % c)
        else:
            parts.append(chr(c))
    parts.append(chr(quote))
    return "".join(parts)


def bytes_concat(left, right, new=False):
    # PyBytes_Concat(&left, right): either side NULL -> result NULL
    # (surfaced as None by the fixture); both sides via PyBUF_SIMPLE.
    if left is None or right is None:
        return None
    try:
        va = _as_simple_bytes(left)
        vb = _as_simple_bytes(right)
    except (TypeError, BufferError):
        raise TypeError(
            "can't concat %s to %s"
            % (type(right).__name__, type(left).__name__)
        ) from None
    return va + vb


def bytes_concatanddel(left, right, new=False):
    # PyBytes_ConcatAndDel(): identical observable behavior.
    return bytes_concat(left, right, new)


def _is_hex_digit(c):
    return (
        0x30 <= c <= 0x39 or 0x61 <= c <= 0x66 or 0x41 <= c <= 0x46
    )


def bytes_decodeescape(s, errors=None, size=None):
    # PyBytes_DecodeEscape() — port of _PyBytes_DecodeEscape2 plus the
    # deprecation-warning epilogue.
    if size is None:
        size = 0 if s is None else len(s)
    if size > _ALLOC_LIMIT:
        # _PyBytesWriter overallocation overflow check.
        raise OverflowError("byte string is too long")
    data = b"" if s is None else bytes(s)[:size]
    out = bytearray()
    first_invalid = -1
    i, n = 0, len(data)
    while i < n:
        c = data[i]
        if c != 0x5C:  # backslash
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= n:
            raise ValueError("Trailing \\ in string")
        c = data[i]
        i += 1
        if c == 0x0A:  # backslash-newline is swallowed
            pass
        elif c == 0x5C:
            out.append(0x5C)
        elif c == 0x27:
            out.append(0x27)
        elif c == 0x22:
            out.append(0x22)
        elif c == 0x62:  # b
            out.append(0x08)
        elif c == 0x66:  # f
            out.append(0x0C)
        elif c == 0x74:  # t
            out.append(0x09)
        elif c == 0x6E:  # n
            out.append(0x0A)
        elif c == 0x72:  # r
            out.append(0x0D)
        elif c == 0x76:  # v
            out.append(0x0B)
        elif c == 0x61:  # a
            out.append(0x07)
        elif 0x30 <= c <= 0x37:  # octal
            val = c - 0x30
            if i < n and 0x30 <= data[i] <= 0x37:
                val = (val << 3) + data[i] - 0x30
                i += 1
                if i < n and 0x30 <= data[i] <= 0x37:
                    val = (val << 3) + data[i] - 0x30
                    i += 1
            if val > 0o377 and first_invalid == -1:
                first_invalid = val
            out.append(val & 0xFF)
        elif c == 0x78:  # x
            if (
                i + 1 < n
                and _is_hex_digit(data[i])
                and _is_hex_digit(data[i + 1])
            ):
                out.append(int(chr(data[i]) + chr(data[i + 1]), 16))
                i += 2
            else:
                if errors is None or errors == "strict":
                    raise ValueError(
                        "invalid \\x escape at position %d" % (i - 2)
                    )
                elif errors == "replace":
                    out.append(0x3F)  # ?
                elif errors == "ignore":
                    pass
                else:
                    raise ValueError(
                        "decoding error; unknown error handling code: %s"
                        % errors
                    )
                if i < n and _is_hex_digit(data[i]):
                    i += 1
        else:
            if first_invalid == -1:
                first_invalid = c
            out.append(0x5C)
            i -= 1  # reprocess the invalid-escape char literally
    if first_invalid != -1:
        import warnings

        if first_invalid > 0xFF:
            warnings.warn(
                "invalid octal escape sequence '\\%o'" % first_invalid,
                DeprecationWarning,
                stacklevel=2,
            )
        else:
            warnings.warn(
                "invalid escape sequence '\\%s'" % chr(first_invalid),
                DeprecationWarning,
                stacklevel=2,
            )
    return bytes(out)


# ---------------------------------------------------------------------------
# Modules/_testcapi/bytes.c


def bytes_resize(obj, newsize, new):
    # _PyBytes_Resize()
    if not isinstance(obj, bytes) or newsize < 0:
        _bad_internal_call()
    if newsize <= len(obj):
        return bytes(obj[:newsize])
    # Grown region is uninitialized in C; the tests only check length
    # and the preserved prefix.
    return bytes(obj) + b"\x00" * (newsize - len(obj))


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/object.c — Py_GetConstant()

_CONSTANTS = (None, False, True, Ellipsis, NotImplemented, 0, 1, "", b"", ())


def get_constant(constant_id):
    # Py_GetConstant()
    cid = int(constant_id)
    if not 0 <= cid < len(_CONSTANTS):
        raise SystemError("constant id %d is invalid" % cid)
    return _CONSTANTS[cid]


def get_constant_borrowed(constant_id):
    # Py_GetConstantBorrowed()
    return get_constant(constant_id)


# ---------------------------------------------------------------------------
# Modules/_testcapi/object.c — PyObject_Print() and weakref clearing


def call_pyobject_print(obj, filename, print_raw):
    # PyObject_Print(obj, fp, print_raw ? Py_PRINT_RAW : 0)
    with open(filename, "w") as fp:
        fp.write(str(obj) if print_raw is True else repr(obj))
    return None


def pyobject_print_null(filename):
    # PyObject_Print(NULL, fp, 0) writes "<nil>".
    with open(filename, "w") as fp:
        fp.write("<nil>")
    return None


def pyobject_print_noref_object(filename):
    # PyObject_Print() on a refcount-0 object prints "<refcnt 0 at %p>";
    # the fixture returns the same string it wrote.
    correct = "<refcnt 0 at %s>" % hex(id(filename))
    with open(filename, "w") as fp:
        fp.write(correct)
    return correct


def pyobject_print_os_error(filename):
    # The C fixture opens the file read-only so the write fails with
    # an OSError.
    with open(filename, "r") as fp:
        fp.write("'Spam spam spam'")
    return None


def pyobject_clear_weakrefs_no_callbacks(obj):
    # PyUnstable_Object_ClearWeakRefsNoCallbacks(): clear every weak
    # reference to obj without running callbacks.
    import weakref

    for r in weakref.getweakrefs(obj):
        clear = getattr(r, "__clear__", None)
        if clear is not None:
            clear()
    return None


# ---------------------------------------------------------------------------
# Modules/_testcapi/hash.c — PyObject_GenericHash()


def object_generichash(obj):
    return object.__hash__(obj)


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/abstract.c — PyObject_* object protocol


def object_str(arg):
    # PyObject_Str(NULL) -> "<NULL>"
    if arg is None:
        return "<NULL>"
    return str(arg)


def object_repr(arg):
    # PyObject_Repr(NULL) -> "<NULL>"
    if arg is None:
        return "<NULL>"
    return repr(arg)


def object_ascii(arg):
    # PyObject_ASCII(NULL) -> "<NULL>"
    if arg is None:
        return "<NULL>"
    return ascii(arg)


def object_bytes(arg):
    # PyObject_Bytes(NULL) -> b"<NULL>"
    if arg is None:
        return b"<NULL>"
    if type(arg) is bytes:
        return arg
    if not isinstance(arg, (bytes, bytearray)):
        func = getattr(type(arg), "__bytes__", None)
        if func is not None:
            res = func(arg)
            if not isinstance(res, bytes):
                raise TypeError(
                    "__bytes__ returned non-bytes (type %s)"
                    % type(res).__name__
                )
            return res
    return _pybytes_fromobject(arg)


def object_getattr(obj, attr_name):
    # PyObject_GetAttr()
    _check_attr_name(attr_name)
    return getattr(obj, attr_name)


def object_getattrstring(obj, attr_name):
    # PyObject_GetAttrString(): the char* name decodes as UTF-8.
    return getattr(obj, _decode_name(attr_name))


def object_hasattr(obj, attr_name):
    # PyObject_HasAttr(): missing -> 0 silently; any other error is
    # routed to sys.unraisablehook and swallowed.
    try:
        _check_attr_name(attr_name)
        getattr(obj, attr_name)
        return 1
    except AttributeError:
        return 0
    except BaseException as exc:
        _write_unraisable(
            exc,
            "Exception ignored in PyObject_HasAttr(); consider using "
            "PyObject_HasAttrWithError(), PyObject_GetOptionalAttr() "
            "or PyObject_GetAttr()",
            obj,
        )
        return 0


def _setattr_c(obj, attr_name, value):
    # CPython raises AttributeError for objects that reject attribute
    # (un)setting; WeavePy's builtin setattr/delattr surfaces TypeError
    # there. Map that narrow case back to the C API's exception type.
    try:
        if value is _DELETE:
            delattr(obj, attr_name)
        else:
            setattr(obj, attr_name, value)
    except TypeError as exc:
        msg = str(exc)
        if msg.startswith("'%s' object has no attribute" % type(obj).__name__):
            raise AttributeError(msg) from None
        raise


class _Delete:
    pass


_DELETE = _Delete()


def object_setattr(obj, attr_name, value):
    # PyObject_SetAttr(); value NULL deletes the attribute.
    _check_attr_name(attr_name)
    _setattr_c(obj, attr_name, _DELETE if value is None else value)
    return 0


def object_setattrstring(obj, attr_name, value):
    # PyObject_SetAttrString()
    name = _decode_name(attr_name)
    _setattr_c(obj, name, _DELETE if value is None else value)
    return 0


def object_delattr(obj, attr_name):
    # PyObject_DelAttr()
    _check_attr_name(attr_name)
    _setattr_c(obj, attr_name, _DELETE)
    return 0


def object_delattrstring(obj, attr_name):
    # PyObject_DelAttrString()
    _setattr_c(obj, _decode_name(attr_name), _DELETE)
    return 0


# ---------------------------------------------------------------------------
# Modules/_testcapi/abstract.c — optional-attr probes


def object_getoptionalattr(obj, attr_name):
    # PyObject_GetOptionalAttr(): 0 -> the AttributeError type itself.
    _check_attr_name(attr_name)
    try:
        return getattr(obj, attr_name)
    except AttributeError:
        return AttributeError


def object_getoptionalattrstring(obj, attr_name):
    # PyObject_GetOptionalAttrString()
    name = _decode_name(attr_name)
    try:
        return getattr(obj, name)
    except AttributeError:
        return AttributeError


def object_hasattrwitherror(obj, attr_name):
    # PyObject_HasAttrWithError(): only AttributeError means "absent";
    # every other exception propagates.
    _check_attr_name(attr_name)
    try:
        getattr(obj, attr_name)
        return 1
    except AttributeError:
        return 0


def object_hasattrstringwitherror(obj, attr_name):
    # PyObject_HasAttrStringWithError()
    name = _decode_name(attr_name)
    try:
        getattr(obj, name)
        return 1
    except AttributeError:
        return 0


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/abstract.c — PyMapping_*


def mapping_check(obj):
    # PyMapping_Check(NULL) == 0; true when the type has mp_subscript.
    if obj is None:
        return 0
    return 1 if getattr(type(obj), "__getitem__", None) is not None else 0


def mapping_size(obj):
    # PyMapping_Size()
    if obj is None:
        _null_error()
    return len(obj)


def mapping_length(obj):
    # PyMapping_Length()
    return mapping_size(obj)


def object_getitem(mapping, key):
    # PyObject_GetItem()
    if mapping is None or key is None:
        _null_error()
    return mapping[key]


def mapping_getitemstring(mapping, key):
    # PyMapping_GetItemString(): NULL key first, then UTF-8 decode,
    # then the subscript (which nulls on a NULL mapping).
    if key is None:
        _null_error()
    okey = _decode_name(key)
    if mapping is None:
        _null_error()
    return mapping[okey]


def mapping_haskey(mapping, key):
    # PyMapping_HasKey(): missing key -> 0 silently; any other error
    # goes to sys.unraisablehook and 0 is returned.
    try:
        if mapping is None or key is None:
            _null_error()
        mapping[key]
        return 1
    except KeyError:
        return 0
    except BaseException as exc:
        _write_unraisable(
            exc,
            "Exception ignored in PyMapping_HasKey(); consider using "
            "PyMapping_HasKeyWithError(), PyMapping_GetOptionalItem() "
            "or PyObject_GetItem()",
            mapping,
        )
        return 0


def mapping_haskeystring(mapping, key):
    # PyMapping_HasKeyString()
    try:
        if mapping is None or key is None:
            _null_error()
        okey = _decode_name(key)
        mapping[okey]
        return 1
    except KeyError:
        return 0
    except BaseException as exc:
        _write_unraisable(
            exc,
            "Exception ignored in PyMapping_HasKeyString(); consider "
            "using PyMapping_HasKeyStringWithError(), "
            "PyMapping_GetOptionalItemString() or PyMapping_GetItemString()",
            mapping,
        )
        return 0


def mapping_haskeywitherror(mapping, key):
    # PyMapping_HasKeyWithError(): only KeyError means "absent".
    if mapping is None or key is None:
        _null_error()
    try:
        mapping[key]
        return 1
    except KeyError:
        return 0


def mapping_haskeystringwitherror(mapping, key):
    # PyMapping_HasKeyStringWithError()
    if key is None:
        _null_error()
    okey = _decode_name(key)
    if mapping is None:
        _null_error()
    try:
        mapping[okey]
        return 1
    except KeyError:
        return 0


def object_setitem(mapping, key, value):
    # PyObject_SetItem()
    if mapping is None or key is None or value is None:
        _null_error()
    mapping[key] = value
    return 0


def mapping_setitemstring(mapping, key, value):
    # PyMapping_SetItemString()
    if key is None:
        _null_error()
    okey = _decode_name(key)
    if mapping is None or value is None:
        _null_error()
    mapping[okey] = value
    return 0


def object_delitem(mapping, key):
    # PyObject_DelItem()
    if mapping is None or key is None:
        _null_error()
    del mapping[key]
    return 0


def mapping_delitem(mapping, key):
    # PyMapping_DelItem() is PyObject_DelItem().
    return object_delitem(mapping, key)


def mapping_delitemstring(mapping, key):
    # PyMapping_DelItemString()
    if key is None:
        _null_error()
    okey = _decode_name(key)
    if mapping is None:
        _null_error()
    del mapping[okey]
    return 0


def _method_output_as_list(obj, meth):
    # abstract.c method_output_as_list()
    out = getattr(obj, meth)()
    if type(out) is list:
        return out
    try:
        it = iter(out)
    except TypeError:
        raise TypeError(
            "%s.%s() returned a non-iterable (type %s)"
            % (type(obj).__name__, meth, type(out).__name__)
        ) from None
    return list(it)


def mapping_keys(obj):
    # PyMapping_Keys()
    if obj is None:
        _null_error()
    return _method_output_as_list(obj, "keys")


def mapping_values(obj):
    # PyMapping_Values()
    if obj is None:
        _null_error()
    return _method_output_as_list(obj, "values")


def mapping_items(obj):
    # PyMapping_Items()
    if obj is None:
        _null_error()
    return _method_output_as_list(obj, "items")


# ---------------------------------------------------------------------------
# Modules/_testcapi/abstract.c — optional-item probes


def mapping_getoptionalitem(obj, key):
    # PyMapping_GetOptionalItem(): 0 -> the KeyError type itself.
    try:
        return obj[key]
    except KeyError:
        return KeyError


def mapping_getoptionalitemstring(obj, key):
    # PyMapping_GetOptionalItemString()
    if key is None:
        _null_error()
    okey = _decode_name(key)
    try:
        return obj[okey]
    except KeyError:
        return KeyError


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/abstract.c — PySequence_*


def _pysequence_check(obj):
    # PySequence_Check(): sq_item and not a dict.
    return (
        obj is not None
        and not isinstance(obj, dict)
        and getattr(type(obj), "__getitem__", None) is not None
    )


def sequence_check(obj):
    return 1 if _pysequence_check(obj) else 0


def sequence_size(obj):
    # PySequence_Size()
    if obj is None:
        _null_error()
    if isinstance(obj, dict):
        raise TypeError("%s is not a sequence" % type(obj).__name__)
    return len(obj)


def sequence_length(obj):
    # PySequence_Length()
    return sequence_size(obj)


def sequence_concat(seq1, seq2):
    # PySequence_Concat()
    if seq1 is None or seq2 is None:
        _null_error()
    if not _pysequence_check(seq1):
        raise TypeError(
            "'%s' object can't be concatenated" % type(seq1).__name__
        )
    return seq1 + seq2


def _repeat_count(seq, count):
    # sq_repeat implementations fail with MemoryError when
    # size * count overflows Py_ssize_t. For an empty sequence the
    # count is irrelevant; clamp it so a huge count doesn't spin.
    try:
        size = len(seq)
    except TypeError:
        return count
    if size == 0:
        return 0 if count > 0 else count
    if count > 0 and count > (2**63 - 1) // size:
        raise MemoryError
    return count


def sequence_repeat(seq, count):
    # PySequence_Repeat()
    if seq is None:
        _null_error()
    if not _pysequence_check(seq):
        raise TypeError("'%s' object can't be repeated" % type(seq).__name__)
    count = _repeat_count(seq, count)
    return seq * count


def sequence_inplaceconcat(seq1, seq2):
    # PySequence_InPlaceConcat()
    if seq1 is None or seq2 is None:
        _null_error()
    if not _pysequence_check(seq1):
        raise TypeError(
            "'%s' object can't be concatenated" % type(seq1).__name__
        )
    func = getattr(type(seq1), "__iadd__", None)
    if func is not None:
        res = func(seq1, seq2)
        if res is not NotImplemented:
            return res
    return seq1 + seq2


def sequence_inplacerepeat(seq, count):
    # PySequence_InPlaceRepeat()
    if seq is None:
        _null_error()
    if not _pysequence_check(seq):
        raise TypeError("'%s' object can't be repeated" % type(seq).__name__)
    count = _repeat_count(seq, count)
    func = getattr(type(seq), "__imul__", None)
    if func is not None:
        res = func(seq, count)
        if res is not NotImplemented:
            return res
    return seq * count


def sequence_setslice(sequence, i1, i2, obj):
    # PySequence_SetSlice(); a NULL value deletes the slice.
    if sequence is None:
        _null_error()
    if obj is None:
        del sequence[i1:i2]
    else:
        sequence[i1:i2] = obj
    return 0


def sequence_delslice(sequence, i1, i2):
    # PySequence_DelSlice()
    if sequence is None:
        _null_error()
    del sequence[i1:i2]
    return 0


def sequence_count(seq, value):
    # PySequence_Count(): iterator search, comparing with ==.
    if seq is None or value is None:
        _null_error()
    n = 0
    for item in seq:
        if item is value or item == value:
            n += 1
    return n


def sequence_contains(seq, value):
    # PySequence_Contains(): NULL value only errors once a comparison
    # would dereference it (empty sequences return 0).
    if seq is None:
        _null_error()
    for item in seq:
        if value is None:
            _null_error()
        if item is value or item == value:
            return 1
    return 0


def sequence_index(seq, value):
    # PySequence_Index()
    if seq is None or value is None:
        _null_error()
    i = 0
    for item in seq:
        if item is value or item == value:
            return i
        i += 1
    raise ValueError("sequence.index(x): x not in sequence")


def sequence_list(obj):
    # PySequence_List()
    if obj is None:
        _null_error()
    return list(obj)


def sequence_tuple(obj):
    # PySequence_Tuple()
    if obj is None:
        _null_error()
    return tuple(obj)
