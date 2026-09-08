"""`_types` -- the static built-in type objects (CPython 3.14's
`Modules/_typesmodule.c`).

`types.py` takes `from _types import *` first and only falls back to its
pure-Python discovery when that import fails; `test_types.test_names`
imports `types` both ways and asserts the two agree. WeavePy has no C
type table, so the same discovery runs here and the module publishes
exactly the names the C module exports (plus `CapsuleType`).
"""

import sys as _sys


def _f(): pass
FunctionType = type(_f)
LambdaType = type(lambda: None)  # Same as FunctionType
CodeType = type(_f.__code__)
MappingProxyType = type(type.__dict__)
SimpleNamespace = type(_sys.implementation)


def _cell_factory():
    a = 1
    def f():
        nonlocal a
    return f.__closure__[0]
CellType = type(_cell_factory())


def _g():
    yield 1
GeneratorType = type(_g())


async def _c(): pass
_c = _c()
CoroutineType = type(_c)
_c.close()  # Prevent ResourceWarning


async def _ag():
    yield
_ag = _ag()
AsyncGeneratorType = type(_ag)


class _C:
    def _m(self): pass
MethodType = type(_C()._m)

BuiltinFunctionType = type(len)
BuiltinMethodType = type([].append)  # Same as BuiltinFunctionType

WrapperDescriptorType = type(object.__init__)
MethodWrapperType = type(object().__str__)
MethodDescriptorType = type(str.join)
ClassMethodDescriptorType = type(dict.__dict__['fromkeys'])

ModuleType = type(_sys)

try:
    raise TypeError
except TypeError as _exc:
    TracebackType = type(_exc.__traceback__)
    FrameType = type(_exc.__traceback__.tb_frame)
    del _exc

GetSetDescriptorType = type(FunctionType.__code__)
MemberDescriptorType = type(FunctionType.__globals__)

GenericAlias = type(list[int])
UnionType = type(int | str)

EllipsisType = type(Ellipsis)
NoneType = type(None)
NotImplementedType = type(NotImplemented)

from _weave_capsule import PyCapsule as CapsuleType

del _sys, _f, _g, _C, _c, _ag, _cell_factory  # Not for export
