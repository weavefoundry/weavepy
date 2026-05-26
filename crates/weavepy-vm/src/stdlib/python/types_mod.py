"""Lightweight stand-in for CPython's `types` module.

This file ships frozen with WeavePy. The exposed surface mirrors the
public API of CPython's ``Lib/types.py`` closely enough that the
standard library can ``import types`` without conditional imports; full
fidelity is not yet a goal (some helpers are simplified — see
:class:`SimpleNamespace`, :func:`new_class`).
"""

import sys as _sys

__all__ = [
    "FunctionType",
    "LambdaType",
    "CodeType",
    "MappingProxyType",
    "SimpleNamespace",
    "GeneratorType",
    "CoroutineType",
    "AsyncGeneratorType",
    "MethodType",
    "BuiltinFunctionType",
    "BuiltinMethodType",
    "WrapperDescriptorType",
    "MethodWrapperType",
    "MethodDescriptorType",
    "ClassMethodDescriptorType",
    "ModuleType",
    "TracebackType",
    "FrameType",
    "GetSetDescriptorType",
    "MemberDescriptorType",
    "CellType",
    "NoneType",
    "NotImplementedType",
    "EllipsisType",
    "UnionType",
    "GenericAlias",
    "coroutine",
    "new_class",
    "prepare_class",
    "resolve_bases",
    "DynamicClassAttribute",
]


# --- type aliases sourced from the running interpreter ------------------
def _f():
    pass


FunctionType = type(_f)
LambdaType = type(lambda: None)
CodeType = type(_f.__code__)


def _g():
    yield 1


GeneratorType = type(_g())


async def _c():
    pass


_coro = _c()
try:
    CoroutineType = type(_coro)
finally:
    try:
        _coro.close()
    except Exception:
        pass


async def _ag():
    yield 1


_a = _ag()
try:
    AsyncGeneratorType = type(_a)
finally:
    try:
        _a.aclose()
    except Exception:
        pass


class _C:
    def _m(self):
        pass


MethodType = type(_C()._m)
BuiltinFunctionType = type(len)
BuiltinMethodType = BuiltinFunctionType

# Several CPython-specific descriptor types don't have direct
# equivalents in WeavePy yet. We resolve them to ``type(None)`` rather
# than raising at import time so that ``import types`` succeeds and
# ``isinstance(x, types.WrapperDescriptorType)`` is a (correctly) False
# check.
def _safe_type(expr_lambda, fallback=type(None)):
    try:
        return type(expr_lambda())
    except Exception:
        return fallback


WrapperDescriptorType = _safe_type(lambda: object.__init__)
MethodWrapperType = _safe_type(lambda: object().__str__)
MethodDescriptorType = _safe_type(lambda: str.join)
ClassMethodDescriptorType = _safe_type(lambda: dict.__dict__.get("fromkeys", classmethod(lambda *a: None)))
ModuleType = type(_sys)
TracebackType = _safe_type(lambda: getattr(getattr(_sys, "exc_info", lambda: (None, None, None))()[2], "__class__", type(None)))
FrameType = _safe_type(lambda: _sys._getframe()) if hasattr(_sys, "_getframe") else type(None)
GetSetDescriptorType = _safe_type(lambda: type.__dict__.get("__dict__", object))
MemberDescriptorType = GetSetDescriptorType
CellType = _safe_type(lambda: (lambda x: (lambda: x))(1).__closure__[0])

NoneType = type(None)
NotImplementedType = type(NotImplemented)
EllipsisType = type(Ellipsis)

UnionType = _safe_type(lambda: int | str)
GenericAlias = _safe_type(lambda: list[int])


class MappingProxyType:
    """Read-only view over a mapping. Mirrors :class:`types.MappingProxyType`."""

    __slots__ = ("_mapping",)

    def __init__(self, mapping):
        # CPython checks for the mapping protocol via slot lookups; we
        # accept anything that's a dict or that supplies the basics.
        if isinstance(mapping, dict):
            self._mapping = mapping
            return
        try:
            mapping.keys
            mapping.__getitem__
        except AttributeError as exc:
            raise TypeError("mappingproxy() argument must support the mapping protocol") from exc
        self._mapping = mapping

    def __getitem__(self, key):
        return self._mapping[key]

    def __contains__(self, key):
        return key in self._mapping

    def __iter__(self):
        return iter(self._mapping)

    def __len__(self):
        return len(self._mapping)

    def __eq__(self, other):
        if isinstance(other, MappingProxyType):
            return self._mapping == other._mapping
        return self._mapping == other

    def __ne__(self, other):
        return not self == other

    def __repr__(self):
        return f"mappingproxy({self._mapping!r})"

    def __or__(self, other):
        if isinstance(other, MappingProxyType):
            return {**self._mapping, **other._mapping}
        return {**self._mapping, **other}

    def __ror__(self, other):
        if isinstance(other, MappingProxyType):
            return {**other._mapping, **self._mapping}
        return {**other, **self._mapping}

    def get(self, key, default=None):
        return self._mapping.get(key, default)

    def keys(self):
        return self._mapping.keys()

    def values(self):
        return self._mapping.values()

    def items(self):
        return self._mapping.items()

    def copy(self):
        if hasattr(self._mapping, "copy"):
            return self._mapping.copy()
        return dict(self._mapping)


class SimpleNamespace:
    """A simple attribute-bag type. Mirrors :class:`types.SimpleNamespace`."""

    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)

    def __repr__(self):
        try:
            keys = sorted(vars(self))
            items = ", ".join(f"{k}={getattr(self, k)!r}" for k in keys)
            return f"namespace({items})"
        except Exception:
            return "namespace(...)"

    def __eq__(self, other):
        if isinstance(self, SimpleNamespace) and isinstance(other, SimpleNamespace):
            return vars(self) == vars(other)
        return NotImplemented

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result


class DynamicClassAttribute:
    """Minimal stand-in for ``types.DynamicClassAttribute``.

    Behaves like a property on instances; on the class itself, raises
    :class:`AttributeError`.
    """

    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        self.fget = fget
        self.fset = fset
        self.fdel = fdel
        if doc is None and fget is not None:
            doc = fget.__doc__
        self.__doc__ = doc
        self.overwrite_doc = doc is None

    def __get__(self, instance, owner=None):
        if instance is None:
            raise AttributeError()
        if self.fget is None:
            raise AttributeError("unreadable attribute")
        return self.fget(instance)

    def __set__(self, instance, value):
        if self.fset is None:
            raise AttributeError("can't set attribute")
        self.fset(instance, value)

    def __delete__(self, instance):
        if self.fdel is None:
            raise AttributeError("can't delete attribute")
        self.fdel(instance)

    def getter(self, fget):
        return type(self)(fget, self.fset, self.fdel, self.__doc__)

    def setter(self, fset):
        return type(self)(self.fget, fset, self.fdel, self.__doc__)

    def deleter(self, fdel):
        return type(self)(self.fget, self.fset, fdel, self.__doc__)


def coroutine(func):
    """Mark a generator function so it can be used with `await`.

    CPython's full implementation rewires the function's flags; here we
    simply return the function unchanged. Most generator-based
    coroutines in modern code use ``async def`` directly.
    """
    return func


def resolve_bases(bases):
    """PEP 560 helper — replace `__mro_entries__` results in *bases*."""
    new_bases = list(bases)
    updated = False
    shift = 0
    for i, base in enumerate(bases):
        if isinstance(base, type):
            continue
        if not hasattr(base, "__mro_entries__"):
            continue
        new = base.__mro_entries__(bases)
        if not isinstance(new, tuple):
            raise TypeError("__mro_entries__ must return a tuple")
        new_bases[i + shift : i + shift + 1] = new
        shift += len(new) - 1
        updated = True
    return tuple(new_bases) if updated else bases


def prepare_class(name, bases=(), kwds=None):
    """PEP 3115 helper — compute metaclass + namespace before class body runs."""
    if kwds is None:
        kwds = {}
    else:
        kwds = dict(kwds)
    if "metaclass" in kwds:
        meta = kwds.pop("metaclass")
    else:
        meta = type(bases[0]) if bases else type
    if isinstance(meta, type):
        meta = _calculate_meta(meta, bases)
    if hasattr(meta, "__prepare__"):
        ns = meta.__prepare__(name, bases, **kwds)
    else:
        ns = {}
    return meta, ns, kwds


def _calculate_meta(meta, bases):
    """Return the most-derived metaclass implied by *meta* and *bases*."""
    winner = meta
    for base in bases:
        base_meta = type(base)
        if issubclass(winner, base_meta):
            continue
        if issubclass(base_meta, winner):
            winner = base_meta
            continue
        raise TypeError(
            "metaclass conflict: the metaclass of a derived class must be "
            "a (non-strict) subclass of the metaclasses of all its bases"
        )
    return winner


def new_class(name, bases=(), kwds=None, exec_body=None):
    """PEP 3115 dynamic class construction helper."""
    resolved = resolve_bases(bases)
    meta, ns, kwds = prepare_class(name, resolved, kwds)
    if exec_body is not None:
        exec_body(ns)
    if resolved is not bases:
        ns["__orig_bases__"] = bases
    return meta(name, resolved, ns, **kwds)


def _cell_factory():
    """Internal helper used by tests to build a freshly-empty ``cell``."""
    a = 1

    def f():
        nonlocal a
        a = 2
        return a

    return f.__closure__[0]


# Cleanup helper names so the module's public surface stays clean.
del _f, _g, _c, _ag, _a, _C, _coro, _safe_type, _sys
