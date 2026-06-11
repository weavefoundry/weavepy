"""WeavePy `weakref` — RFC 0024 real weakrefs.

After RFC 0024, `_weakref` is backed by a real per-thread weakref
registry coordinated with the cycle GC. Calling a `ref` returns
`None` once the GC has cleared the referent; `proxy` raises
`ReferenceError`. `WeakValueDictionary` / `WeakKeyDictionary` /
`WeakSet` decay their entries when their referents die.

The thin shims that previously held a strong reference (RFC 0018)
are gone — `ref(obj)` / `proxy(obj)` now delegate directly to
`_weakref.ref` / `_weakref.proxy`.
"""

import _weakref


__all__ = [
    "ref",
    "proxy",
    "getweakrefcount",
    "getweakrefs",
    "WeakValueDictionary",
    "WeakKeyDictionary",
    "WeakSet",
    "WeakMethod",
    "finalize",
    "ReferenceType",
    "ProxyType",
    "CallableProxyType",
    "ProxyTypes",
]


ReferenceType = _weakref.ReferenceType
ProxyType = _weakref.ProxyType
CallableProxyType = _weakref.CallableProxyType
ProxyTypes = (ProxyType, CallableProxyType)


def getweakrefcount(obj):
    return _weakref.getweakrefcount(obj)


def getweakrefs(obj):
    return _weakref.getweakrefs(obj)


# `ref` IS the native ReferenceType, exactly as in CPython. Calling it
# builds a real weakref; `obj.__weakref__` and `getweakrefs(obj)` hand
# back the same object.
ref = _weakref.ref


def proxy(target, callback=None):
    """Construct a weakly-bound proxy to `target`. Attribute /
    item / call access goes to the live target while it is
    reachable; once cleared, every operation raises
    `ReferenceError`. If `target` is callable the proxy is
    callable too (`CallableProxyType`)."""
    return _weakref.proxy(target, callback)


class WeakMethod:
    """Weak reference to a bound method.

    Holds the underlying object weakly via `weakref.ref`; calls
    to the WeakMethod return the bound method *if* the object is
    still alive, or `None` once the cycle GC has cleared it.
    """

    __slots__ = ("_obj_ref", "_func", "_meth_type")

    def __init__(self, meth, callback=None):
        obj = meth.__self__
        func = meth.__func__
        self._obj_ref = ref(obj, callback)
        self._func = func
        self._meth_type = type(meth)

    def __call__(self):
        obj = self._obj_ref()
        if obj is None:
            return None
        try:
            return self._meth_type(self._func, obj)
        except TypeError:
            return getattr(obj, self._func.__name__)


class WeakValueDictionary:
    """Dict whose values are weakly held.

    Users wishing to evict an entry should call `expire(key)`.
    Iteration / lookup transparently filters out dead refs.
    """

    def __init__(self, mapping=None, /, **kwargs):
        self._data = {}
        if mapping is not None:
            self.update(mapping)
        if kwargs:
            self.update(kwargs)

    def _wrap(self, key, value):
        def _on_finalise(_):
            self._data.pop(key, None)

        return ref(value, _on_finalise)

    def __getitem__(self, key):
        r = self._data[key]
        value = r()
        if value is None:
            del self._data[key]
            raise KeyError(key)
        return value

    def __setitem__(self, key, value):
        self._data[key] = self._wrap(key, value)

    def __delitem__(self, key):
        del self._data[key]

    def __iter__(self):
        for key, r in list(self._data.items()):
            if r() is not None:
                yield key

    def __len__(self):
        return sum(1 for _ in self)

    def __contains__(self, key):
        try:
            self[key]
            return True
        except KeyError:
            return False

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def setdefault(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            self[key] = default
            return default

    def keys(self):
        return list(iter(self))

    def values(self):
        return [self[k] for k in self.keys()]

    def items(self):
        return [(k, self[k]) for k in self.keys()]

    def update(self, *args, **kwargs):
        if args:
            other = args[0]
            if hasattr(other, "keys"):
                for k in other.keys():
                    self[k] = other[k]
            else:
                for k, v in other:
                    self[k] = v
        for k, v in kwargs.items():
            self[k] = v

    def pop(self, key, *args):
        try:
            value = self[key]
        except KeyError:
            if args:
                return args[0]
            raise
        del self._data[key]
        return value

    def popitem(self):
        while self._data:
            key, r = self._data.popitem()
            v = r()
            if v is not None:
                return key, v
        raise KeyError("dictionary is empty")

    def clear(self):
        self._data.clear()

    def copy(self):
        new = WeakValueDictionary()
        for k in self.keys():
            new[k] = self[k]
        return new

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy

        new = WeakValueDictionary()
        for k in self.keys():
            new[deepcopy(k, memo)] = self[k]
        return new

    def __eq__(self, other):
        # Mirror `_collections_abc.Mapping.__eq__`: two weak mappings are
        # equal iff their *live* items compare equal as plain dicts. Needed
        # so `copy.copy(wd) == wd` (test_copy) holds.
        if not isinstance(other, WeakValueDictionary):
            return NotImplemented
        return dict(self.items()) == dict(other.items())

    __hash__ = None

    def expire(self, key):
        self._data.pop(key, None)

    def valuerefs(self):
        return list(self._data.values())


class WeakKeyDictionary:
    """Dict whose keys are weakly held.

    Implemented as a list of (ref, value) pairs because dict keys
    must be hashable in a stable way — the ref's identity can change
    once released so we re-resolve manually.
    """

    def __init__(self, mapping=None):
        self._entries = []
        if mapping is not None:
            self.update(mapping)

    def _find(self, key):
        for i, (kref, _value) in enumerate(self._entries):
            target = kref()
            if target is None:
                continue
            if target is key or target == key:
                return i
        return -1

    def __getitem__(self, key):
        i = self._find(key)
        if i < 0:
            raise KeyError(key)
        return self._entries[i][1]

    def __setitem__(self, key, value):
        i = self._find(key)
        if i >= 0:
            self._entries[i] = (self._entries[i][0], value)
            return
        kref = ref(key, lambda _r: self._scrub())
        self._entries.append((kref, value))

    def __delitem__(self, key):
        i = self._find(key)
        if i < 0:
            raise KeyError(key)
        self._entries.pop(i)

    def _scrub(self):
        self._entries = [(k, v) for (k, v) in self._entries if k() is not None]

    def __iter__(self):
        for kref, _ in self._entries:
            t = kref()
            if t is not None:
                yield t

    def __len__(self):
        return sum(1 for _ in self)

    def __contains__(self, key):
        return self._find(key) >= 0

    def get(self, key, default=None):
        i = self._find(key)
        if i < 0:
            return default
        return self._entries[i][1]

    def keys(self):
        return list(iter(self))

    def values(self):
        return [v for (k, v) in self._entries if k() is not None]

    def items(self):
        out = []
        for kref, v in self._entries:
            t = kref()
            if t is not None:
                out.append((t, v))
        return out

    def update(self, *args, **kwargs):
        if args:
            other = args[0]
            if hasattr(other, "keys"):
                for k in other.keys():
                    self[k] = other[k]
            else:
                for k, v in other:
                    self[k] = v
        for k, v in kwargs.items():
            self[k] = v

    def pop(self, key, *args):
        i = self._find(key)
        if i < 0:
            if args:
                return args[0]
            raise KeyError(key)
        _, v = self._entries.pop(i)
        return v

    def clear(self):
        self._entries.clear()

    def copy(self):
        new = WeakKeyDictionary()
        for k, v in self.items():
            new[k] = v
        return new

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy

        new = WeakKeyDictionary()
        for key, value in self.items():
            new[key] = deepcopy(value, memo)
        return new

    def __eq__(self, other):
        # See WeakValueDictionary.__eq__ — equal iff live items match.
        if not isinstance(other, WeakKeyDictionary):
            return NotImplemented
        return dict(self.items()) == dict(other.items())

    __hash__ = None

    def keyrefs(self):
        return [k for (k, _) in self._entries]


class WeakSet:
    """Set whose members are weakly held."""

    def __init__(self, data=None):
        self._refs = []
        if data is not None:
            for x in data:
                self.add(x)

    def _scrub(self):
        self._refs = [r for r in self._refs if r() is not None]

    def __iter__(self):
        for r in list(self._refs):
            t = r()
            if t is not None:
                yield t

    def __len__(self):
        return sum(1 for _ in self)

    def __contains__(self, item):
        for r in self._refs:
            t = r()
            if t is item or t == item:
                return True
        return False

    def add(self, item):
        if item in self:
            return
        self._refs.append(ref(item, lambda _r: self._scrub()))

    def discard(self, item):
        for i, r in enumerate(self._refs):
            t = r()
            if t is item or t == item:
                self._refs.pop(i)
                return

    def remove(self, item):
        before = len(self._refs)
        self.discard(item)
        if len(self._refs) == before:
            raise KeyError(item)

    def clear(self):
        self._refs.clear()

    def copy(self):
        new = WeakSet()
        for x in self:
            new.add(x)
        return new

    def update(self, other):
        for x in other:
            self.add(x)

    def difference_update(self, other):
        for x in list(other):
            self.discard(x)

    def union(self, other):
        new = self.copy()
        new.update(other)
        return new

    def __ior__(self, other):
        self.update(other)
        return self


_finalizer_counter = 0
_finalizer_registry = {}


class finalize:
    """Cooperative finalizer.

    Registers a callable to invoke when `_release()` is called on the
    associated object. The runtime can't fire it automatically; user
    code must invoke `.detach()` / `()` / `_release()` explicitly.
    """

    __slots__ = ("_index", "_func", "_args", "_kwargs", "_alive", "atexit")

    def __init__(self, obj, func, /, *args, **kwargs):
        global _finalizer_counter
        _finalizer_counter += 1
        self._index = _finalizer_counter
        self._func = func
        self._args = args
        self._kwargs = kwargs
        self._alive = True
        self.atexit = True
        _finalizer_registry[self._index] = self

    def __call__(self, _=None):
        if not self._alive:
            return None
        self._alive = False
        _finalizer_registry.pop(self._index, None)
        try:
            return self._func(*self._args, **self._kwargs)
        except Exception:
            return None

    def detach(self):
        if not self._alive:
            return None
        self._alive = False
        _finalizer_registry.pop(self._index, None)
        return (self._func, self._args, self._kwargs)

    def peek(self):
        if not self._alive:
            return None
        return (self._func, self._args, self._kwargs)

    @property
    def alive(self):
        return self._alive
