"""Pure-Python stand-in for CPython's ``_testlimitedcapi`` test helper.

CPython's test suite reaches for this C extension to exercise the public
*abstract* object protocol from C. WeavePy has no C extensions, so we
provide faithful Python equivalents of the handful of wrappers the
conformance targets actually use. Each mirrors the corresponding
``PySequence_*`` C-API call, which for the built-in sequence types under
test is plain subscripting.
"""


def sequence_getitem(obj, i):
    # PySequence_GetItem(obj, i)
    return obj[i]


def sequence_setitem(obj, i, value):
    # PySequence_SetItem(obj, i, value)
    obj[i] = value


def sequence_delitem(obj, i):
    # PySequence_DelItem(obj, i)
    del obj[i]


def object_hasattrstring(obj, name):
    # PyObject_HasAttrString(obj, name) — `name` arrives as bytes
    # (a C `char*`); returns 1/0.
    if isinstance(name, (bytes, bytearray)):
        name = name.decode("utf-8")
    return 1 if hasattr(obj, name) else 0


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
