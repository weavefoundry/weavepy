"""Direct and C API Union subscriptions agree with Python subscription."""

import ctypes
from types import UnionType
from typing import ForwardRef, Literal, Union

assert Union is UnionType
method = Union.__class_getitem__
assert method.__self__ is Union
assert method.__name__ == '__class_getitem__'

getitem = ctypes.pythonapi.PyObject_GetItem
getitem.argtypes = (ctypes.py_object, ctypes.py_object)
getitem.restype = ctypes.py_object

# The forward references and Literal mirror native Cython declarations in
# SQLAlchemy. Its optimized call uses the method directly, with one tuple.
for key in [
    (int, str),
    (int, int),
    (int, None),
    (int | str, float),
    (int, str, 'CacheConst'),
    (int, Literal[True], '_CoreSingleExecuteParams'),
]:
    expected = Union[key]
    assert method(key) == expected
    assert Union.__class_getitem__(key) == expected
    assert UnionType.__class_getitem__(key) == expected
    assert getitem(Union, key) == expected

assert method(int) is int
assert method('Example') == ForwardRef('Example')
for args in [(), (int, str), ((int, str), float), ((),)]:
    try:
        method(*args)
    except TypeError:
        pass
    else:
        raise AssertionError(('invalid arguments accepted', args))
try:
    method(item=int)
except TypeError:
    pass
else:
    raise AssertionError('keyword accepted')

print('Union method binding, arguments, and subscription: ok')
