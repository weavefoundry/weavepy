"""C API class attribute lookup must run custom descriptors."""

import ctypes
import sys

getattr_c = ctypes.pythonapi.PyObject_GetAttr
getattr_c.argtypes = (ctypes.py_object, ctypes.py_object)
getattr_c.restype = ctypes.py_object
getattr_string_c = ctypes.pythonapi.PyObject_GetAttrString
getattr_string_c.argtypes = (ctypes.py_object, ctypes.c_char_p)
getattr_string_c.restype = ctypes.py_object

calls = []


class Descriptor:
    def __get__(self, instance, owner):
        calls.append((instance, owner))
        return owner.__name__


class Meta(type):
    pass


class Base(metaclass=Meta):
    value = Descriptor()


class Child(Base):
    pass


for cls in (Base, Child):
    for get, name in ((getattr_c, 'value'), (getattr_string_c, b'value')):
        calls.clear()
        assert get(cls, name) == cls.value == cls.__name__
        assert calls == [(None, cls), (None, cls)]


class Hybrid:
    def __get__(self, instance, owner):
        def method(value):
            return owner, instance, value
        return method


Base.method = Hybrid()
assert getattr_c(Child, 'method')(17) == (Child, None, 17)


def changed(self, instance, owner):
    return ('changed', owner)


Descriptor.__get__ = changed
assert getattr_c(Child, 'value') == ('changed', Child)


class Raising:
    def __get__(self, instance, owner):
        raise ValueError('descriptor failed')


Base.bad = Raising()
for get, name in ((getattr_c, 'bad'), (getattr_string_c, b'bad')):
    try:
        get(Child, name)
    except ValueError as exc:
        assert str(exc) == 'descriptor failed'
    else:
        raise AssertionError('descriptor error lost')

# Optional lookups must distinguish a descriptor result, AttributeError,
# and other exceptions. Compare and release their new references through
# the C API, without depending on ctypes' PyObject pointer-cast support.
equal = ctypes.pythonapi.PyObject_RichCompareBool
equal.argtypes = (ctypes.c_void_p, ctypes.py_object, ctypes.c_int)
equal.restype = ctypes.c_int
decref = ctypes.pythonapi.Py_DecRef
decref.argtypes = (ctypes.c_void_p,)
decref.restype = None
optional = [
    (ctypes.pythonapi.PyObject_GetOptionalAttr, False, False),
    (ctypes.pythonapi.PyObject_GetOptionalAttrString, True, False),
]
if hasattr(ctypes.pythonapi, '_PyObject_LookupAttr'):
    optional.append((ctypes.pythonapi._PyObject_LookupAttr, False,
                     sys.implementation.name == 'weavepy'))


class Missing:
    def __get__(self, instance, owner):
        raise AttributeError('missing through descriptor')


Base.missing_descriptor = Missing()
for lookup, string_name, permits_null_result in optional:
    lookup.argtypes = (
        ctypes.py_object,
        ctypes.c_char_p if string_name else ctypes.py_object,
        ctypes.POINTER(ctypes.c_void_p),
    )
    lookup.restype = ctypes.c_int
    name = lambda value: value.encode() if string_name else value
    result = ctypes.c_void_p()
    assert lookup(Child, name('value'), ctypes.byref(result)) == 1
    assert result.value
    try:
        assert equal(result, ('changed', Child), 2) == 1
    finally:
        decref(result)
    # WeavePy's legacy private helper also accepts a presence-only probe.
    # CPython's public API requires a non-null output pointer.
    if permits_null_result:
        assert lookup(Child, name('value'), None) == 1
    for missing in ('absent', 'missing_descriptor'):
        result = ctypes.c_void_p(1)
        assert lookup(Child, name(missing), ctypes.byref(result)) == 0
        assert result.value is None
    result = ctypes.c_void_p(1)
    try:
        lookup(Child, name('bad'), ctypes.byref(result))
    except ValueError as exc:
        assert str(exc) == 'descriptor failed'
        assert result.value is None
    else:
        raise AssertionError('optional lookup lost descriptor error')

print('C API class descriptors, inheritance, mutation, and errors: ok')
