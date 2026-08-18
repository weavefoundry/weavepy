"""Pure-Python stand-in for CPython's ``_testlimitedcapi`` test helper.

CPython's test suite reaches for this C extension to exercise the public
*abstract* object protocol from C. WeavePy has no C extensions, so we
provide faithful Python equivalents of the handful of wrappers the
conformance targets actually use. Each mirrors the corresponding
``PySequence_*`` C-API call, which for the built-in sequence types under
test is plain subscripting.
"""


def _sequence_check(obj):
    # The PySequence_{Get,Set,Del}Item prologue: NULL is a SystemError,
    # and an object whose type has `mp_subscript` but no `sq_item` (a
    # mapping — dict is the test's canonical case) is a TypeError, not
    # a KeyError from falling through to `obj[i]`
    # (test_capi.test_abstract).
    if obj is None:
        raise SystemError("null argument to internal routine")
    import collections.abc

    if isinstance(obj, (dict, collections.abc.Mapping)):
        raise TypeError("%.200s is not a sequence" % type(obj).__name__)


def sequence_getitem(obj, i):
    # PySequence_GetItem(obj, i)
    _sequence_check(obj)
    return obj[i]


def sequence_setitem(obj, i, value):
    # PySequence_SetItem(obj, i, value); a NULL value (None sentinel at
    # the test boundary) deletes the item, like the C fixture's
    # `PySequence_SetItem(obj, i, NULL)`.
    _sequence_check(obj)
    if value is None:
        del obj[i]
    else:
        obj[i] = value


def sequence_delitem(obj, i):
    # PySequence_DelItem(obj, i)
    _sequence_check(obj)
    del obj[i]


def object_hasattrstring(obj, name):
    # PyObject_HasAttrString(obj, name) — `name` arrives as bytes
    # (a C `char*`); returns 1/0. Any error other than AttributeError
    # (a raising property, undecodable name bytes) is routed to
    # sys.unraisablehook and swallowed, per 3.13's PyObject_HasAttr
    # semantics (test_capi.test_abstract test_object_hasattrstring).
    try:
        if isinstance(name, (bytes, bytearray)):
            name = bytes(name).decode("utf-8")
        getattr(obj, name)
        return 1
    except AttributeError:
        return 0
    except BaseException as exc:
        from _weave_capi_bin import _write_unraisable

        _write_unraisable(
            exc,
            "Exception ignored in PyObject_HasAttrString(); consider "
            "using PyObject_HasAttrStringWithError(), "
            "PyObject_GetOptionalAttrString() or PyObject_GetAttrString()",
            obj,
        )
        return 0


# --- RFC 0060: limited-API vectorcall probes (test_call.TestPEP590) ------

class LimitedVectorCallClass:
    """A limited-API type whose vectorcall function answers the fixed
    string; incoming vectorcalls (`PyObject_Vectorcall(obj, ...)`) land
    on the same call path in WeavePy."""

    def __call__(self):
        return "vectorcall called"


def call_vectorcall(callable):
    # PyObject_Vectorcall(callable, ["foo"], 1 args + kwname "baz"="bar")
    return callable("foo", baz="bar")


def call_vectorcall_method(obj):
    # PyObject_VectorcallMethod("f", [obj, "foo"], ..., kwname "baz")
    return obj.f("foo", baz="bar")


# RFC 0068 WS3 — per-family C-API fixture shims (test_capi per-leg
# suites). Each module defines `__all__`; the star imports splice its
# fixtures into this namespace. Kept last so nothing above is shadowed.
class _CFuncLtd:
    """Non-descriptor callable: test_misc splices `test_*` fixtures into
    a TestCase namespace (`locals().update(get_test_funcs(...))`), where
    a plain function would wrongly bind `self` like a method. A C
    builtin doesn't; neither does this wrapper."""

    def __init__(self, fn):
        self._fn = fn
        self.__name__ = getattr(fn, "__name__", "cfunc")

    def __call__(self, *args, **kwargs):
        return self._fn(*args, **kwargs)


def _test_widechar_impl():
    # Modules/_testlimitedcapi/unicode.c test_widechar — the 4-byte
    # wchar_t branch (unix): PyUnicode_FromWideChar of U+10ABCD must
    # equal the UTF-8 decode of \xf4\x8a\xaf\x8d, and a wide char past
    # U+10FFFF must be rejected.
    wide = "\U0010ABCD"
    utf8 = b"\xf4\x8a\xaf\x8d".decode("utf-8")
    if len(wide) != len(utf8):
        raise AssertionError(
            "test_widechar: wide string and utf8 string have different length"
        )
    if wide != utf8:
        raise AssertionError(
            "test_widechar: wide string and utf8 string are different"
        )
    try:
        chr(0x110000)
    except ValueError:
        pass
    else:
        raise AssertionError(
            "test_widechar: "
            'PyUnicode_FromWideChar(L"\\U00110000", 1) didn\'t fail'
        )
    return None


test_widechar = _CFuncLtd(_test_widechar_impl)


from _weave_capi_bin import *  # noqa: E402,F401,F403
from _weave_capi_cont import *  # noqa: E402,F401,F403
from _weave_capi_num import *  # noqa: E402,F401,F403
from _weave_capi_text import *  # noqa: E402,F401,F403
from _weave_capi_misc import *  # noqa: E402,F401,F403
