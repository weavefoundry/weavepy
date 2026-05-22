"""WeavePy `weakref` — wrapper around the `_weakref` Rust core.

This module mirrors CPython's `weakref` surface area at a level that
makes typical user code Just Work, even though the underlying object
model uses strict reference counting and so cannot, in general,
expose "true" weak references that automatically zero themselves
out. See RFC 0018 for the documented drawback.

In practice this means:

* `ref(obj)`, `proxy(obj)`, `WeakSet`, `WeakValueDictionary`,
  `WeakKeyDictionary`, and `finalize` all work for the cooperative
  pattern where users explicitly drop their last strong reference.
* The runtime will not magically clean these structures when the
  last strong reference disappears; users that need that behaviour
  should drop and reset, or explicitly call `_release()` on the
  weakref.
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


class ref:
    """Cooperative shim weak reference.

    Holds a (strong) reference to its target until `_release()` is
    called. Calling the ref returns the target — or `None` if the
    target has been released.
    """

    __slots__ = ("_target", "_callback", "_dead", "__weakref__")

    def __init__(self, target, callback=None):
        object.__setattr__(self, "_target", target)
        object.__setattr__(self, "_callback", callback)
        object.__setattr__(self, "_dead", False)

    def __call__(self):
        if self._dead:
            return None
        return self._target

    def _release(self):
        if self._dead:
            return
        object.__setattr__(self, "_dead", True)
        cb = self._callback
        object.__setattr__(self, "_target", None)
        if cb is not None:
            try:
                cb(self)
            except Exception:
                pass

    def __repr__(self):
        if self._dead:
            return "<weakref at 0x0; dead>"
        return f"<weakref at 0x0; to '{type(self._target).__name__}'>"

    def __hash__(self):
        return id(self)

    def __eq__(self, other):
        return self is other


class proxy:
    """Attribute-delegating proxy."""

    __slots__ = ("_target", "_callback", "_dead", "__weakref__")

    def __init__(self, target, callback=None):
        object.__setattr__(self, "_target", target)
        object.__setattr__(self, "_callback", callback)
        object.__setattr__(self, "_dead", False)

    def _check(self):
        if self._dead:
            raise ReferenceError("weakly-referenced object no longer exists")
        return self._target

    def _release(self):
        if self._dead:
            return
        object.__setattr__(self, "_dead", True)
        cb = self._callback
        object.__setattr__(self, "_target", None)
        if cb is not None:
            try:
                cb(self)
            except Exception:
                pass

    def __getattr__(self, name):
        return getattr(self._check(), name)

    def __setattr__(self, name, value):
        setattr(self._check(), name, value)

    def __delattr__(self, name):
        delattr(self._check(), name)

    def __repr__(self):
        if self._dead:
            return "<weakproxy; dead>"
        return f"<weakproxy at 0x0 to {type(self._target).__name__}>"

    def __bool__(self):
        return not self._dead and bool(self._target)

    def __len__(self):
        return len(self._check())

    def __getitem__(self, key):
        return self._check()[key]

    def __setitem__(self, key, value):
        self._check()[key] = value

    def __delitem__(self, key):
        del self._check()[key]

    def __contains__(self, item):
        return item in self._check()

    def __iter__(self):
        return iter(self._check())

    def __call__(self, *args, **kwargs):
        return self._check()(*args, **kwargs)


class WeakMethod(ref):
    """Weak reference to a bound method."""

    def __new__(cls, meth, callback=None):
        obj = meth.__self__
        func = meth.__func__
        self = super().__new__(cls)
        ref.__init__(self, obj, callback)
        self._func = func
        self._meth_type = type(meth)
        return self

    def __init__(self, meth, callback=None):
        pass

    def __call__(self):
        obj = super().__call__()
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
