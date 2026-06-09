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
