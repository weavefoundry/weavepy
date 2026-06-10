"""WeavePy's pure-Python ``collections`` module.

The shape mirrors CPython's public API closely enough that everyday
code (``defaultdict(list)``, ``Counter('hello').most_common()``,
``deque(maxlen=3)``) works without modification.

The implementations intentionally favour clarity over micro-optimised
behaviour. They are designed to run on top of WeavePy's own bytecode
interpreter, not to be the fastest possible Python.
"""

__all__ = [
    "deque",
    "OrderedDict",
    "defaultdict",
    "Counter",
    "ChainMap",
    "namedtuple",
    "UserDict",
    "UserList",
    "UserString",
]

# `UserDict`/`UserList`/`UserString` are verbatim CPython and depend on
# `collections.abc`, so they live in a sibling frozen module (imported at
# the end of this file, after the package is otherwise initialised) to keep
# the import graph acyclic.


def _count_elements(mapping, iterable):
    """Tally elements from the iterable.

    The pure-Python fallback CPython ships when the ``_collections``
    C accelerator is unavailable; ``test_collections`` imports it by
    name to exercise ``Counter`` behaviour.
    """
    mapping_get = mapping.get
    for elem in iterable:
        mapping[elem] = mapping_get(elem, 0) + 1


class deque:
    """Double-ended queue with optional maximum length.

    Supports the operations exercised by typical Python code: append,
    appendleft, pop, popleft, extend, extendleft, rotate, clear, copy,
    indexing, iteration, len, contains, and equality.
    """

    def __init__(self, iterable=None, maxlen=None):
        if maxlen is not None and maxlen < 0:
            raise ValueError("maxlen must be non-negative")
        self._data = []
        self._maxlen = maxlen
        if iterable is not None:
            for item in iterable:
                self.append(item)

    @property
    def maxlen(self):
        return self._maxlen

    def append(self, x):
        self._data.append(x)
        if self._maxlen is not None and len(self._data) > self._maxlen:
            del self._data[0]

    def appendleft(self, x):
        self._data.insert(0, x)
        if self._maxlen is not None and len(self._data) > self._maxlen:
            self._data.pop()

    def pop(self):
        if not self._data:
            raise IndexError("pop from an empty deque")
        return self._data.pop()

    def popleft(self):
        if not self._data:
            raise IndexError("pop from an empty deque")
        return self._data.pop(0)

    def extend(self, iterable):
        for item in iterable:
            self.append(item)

    def extendleft(self, iterable):
        for item in iterable:
            self.appendleft(item)

    def rotate(self, n=1):
        if not self._data:
            return
        size = len(self._data)
        n = n % size
        if n == 0:
            return
        self._data = self._data[-n:] + self._data[:-n]

    def clear(self):
        self._data = []

    def copy(self):
        return deque(self._data, self._maxlen)

    def count(self, value):
        return sum(1 for item in self._data if item == value)

    def index(self, value, start=0, stop=None):
        if stop is None:
            stop = len(self._data)
        for i in range(start, stop):
            if self._data[i] == value:
                return i
        raise ValueError(repr(value) + " is not in deque")

    def insert(self, i, x):
        if self._maxlen is not None and len(self._data) >= self._maxlen:
            raise IndexError("deque already at its maximum size")
        self._data.insert(i, x)

    def remove(self, value):
        for i, item in enumerate(self._data):
            if item == value:
                del self._data[i]
                return
        raise ValueError(repr(value) + " not in deque")

    def reverse(self):
        self._data.reverse()

    def __len__(self):
        return len(self._data)

    def __iter__(self):
        return iter(self._data)

    def __reversed__(self):
        return reversed(self._data)

    def __contains__(self, x):
        return x in self._data

    def __getitem__(self, idx):
        return self._data[idx]

    def __setitem__(self, idx, value):
        self._data[idx] = value

    def __eq__(self, other):
        if isinstance(other, deque):
            return self._data == other._data
        return NotImplemented

    def __repr__(self):
        if self._maxlen is None:
            return "deque(" + repr(self._data) + ")"
        return "deque(" + repr(self._data) + ", maxlen=" + repr(self._maxlen) + ")"


class _MappingMixin:
    """Internal helper: provides the common mapping wire-up our pure-
    Python container types share. Composes a plain ``dict`` instead of
    inheriting, sidestepping WeavePy's current lack of MRO dispatch
    onto built-in types."""

    def __init__(self):
        self._data = {}

    def __getitem__(self, key):
        try:
            return self._data[key]
        except KeyError:
            miss = getattr(self, "__missing__", None)
            if miss is not None:
                return miss(key)
            raise

    def __setitem__(self, key, value):
        self._data[key] = value

    def __delitem__(self, key):
        del self._data[key]

    def __contains__(self, key):
        return key in self._data

    def __len__(self):
        return len(self._data)

    def __iter__(self):
        return iter(self._data)

    def keys(self):
        return list(self._data.keys())

    def values(self):
        return list(self._data.values())

    def items(self):
        return list(self._data.items())

    def get(self, key, default=None):
        return self._data.get(key, default)

    def pop(self, key, *args):
        if args:
            return self._data.pop(key, args[0])
        return self._data.pop(key)

    def update(self, other=None, **kwargs):
        if other is not None:
            if hasattr(other, "items"):
                for k, v in other.items():
                    self._data[k] = v
            else:
                for k, v in other:
                    self._data[k] = v
        for k, v in kwargs.items():
            self._data[k] = v

    def clear(self):
        self._data.clear()


class OrderedDict(_MappingMixin):
    """Dict that remembers insertion order.

    WeavePy's built-in ``dict`` already preserves insertion order, so
    this is mostly the additional ``move_to_end`` /
    ``popitem(last=...)`` semantics."""

    def __init__(self, *args, **kwargs):
        _MappingMixin.__init__(self)
        if len(args) > 1:
            raise TypeError(
                f"expected at most 1 positional argument, got {len(args)}"
            )
        if args:
            self.update(args[0])
        if kwargs:
            self.update(kwargs)

    def setdefault(self, key, default=None):
        if key in self._data:
            return self._data[key]
        self._data[key] = default
        return default

    def __eq__(self, other):
        if isinstance(other, OrderedDict):
            return (list(self.items()) == list(other.items()))
        return dict(self) == other

    def __ne__(self, other):
        return not self == other

    def copy(self):
        return OrderedDict(self)

    @classmethod
    def fromkeys(cls, iterable, value=None):
        d = cls()
        for k in iterable:
            d[k] = value
        return d

    def move_to_end(self, key, last=True):
        if key not in self._data:
            raise KeyError(key)
        value = self._data.pop(key)
        if last:
            self._data[key] = value
        else:
            items = [(key, value)]
            for k in list(self._data.keys()):
                items.append((k, self._data.pop(k)))
            for k, v in items:
                self._data[k] = v

    def popitem(self, last=True):
        if not self._data:
            raise KeyError("dictionary is empty")
        keys = list(self._data.keys())
        key = keys[-1] if last else keys[0]
        value = self._data.pop(key)
        return (key, value)

    def __repr__(self):
        items = ", ".join(repr(k) + ": " + repr(v) for k, v in self.items())
        return "OrderedDict({" + items + "})"


class defaultdict(_MappingMixin):
    """Dict that creates missing values via a ``default_factory``."""

    def __init__(self, default_factory=None, *args, **kwargs):
        if default_factory is not None and not callable(default_factory):
            raise TypeError("first argument must be callable or None")
        _MappingMixin.__init__(self)
        self.default_factory = default_factory
        if args:
            src = args[0]
            if hasattr(src, "keys"):
                for k in src.keys():
                    self[k] = src[k]
            else:
                for k, v in src:
                    self[k] = v
        for k, v in kwargs.items():
            self[k] = v

    def __getitem__(self, key):
        if key in self._data:
            return self._data[key]
        if self.default_factory is None:
            raise KeyError(key)
        value = self.default_factory()
        self._data[key] = value
        return value

    def __repr__(self):
        return (
            "defaultdict("
            + repr(self.default_factory)
            + ", "
            + repr(self._data)
            + ")"
        )

    def copy(self):
        new = defaultdict(self.default_factory)
        for k, v in self.items():
            new[k] = v
        return new


class Counter(_MappingMixin):
    """Pure-Python ``Counter`` backed by an internal dict."""

    def __init__(self, iterable=None, **kwargs):
        _MappingMixin.__init__(self)
        if iterable is not None:
            self.update(iterable)
        if kwargs:
            self.update(kwargs)

    def update(self, iterable=None, **kwargs):
        if iterable is not None:
            if hasattr(iterable, "items"):
                for key, value in iterable.items():
                    self._data[key] = self._data.get(key, 0) + value
            else:
                for item in iterable:
                    self._data[item] = self._data.get(item, 0) + 1
        if kwargs:
            for key, value in kwargs.items():
                self._data[key] = self._data.get(key, 0) + value

    def subtract(self, iterable=None, **kwargs):
        if iterable is not None:
            if hasattr(iterable, "items"):
                for key, value in iterable.items():
                    self._data[key] = self._data.get(key, 0) - value
            else:
                for item in iterable:
                    self._data[item] = self._data.get(item, 0) - 1
        if kwargs:
            for key, value in kwargs.items():
                self._data[key] = self._data.get(key, 0) - value

    def most_common(self, n=None):
        items = list(self._data.items())
        items.sort(key=lambda kv: kv[1], reverse=True)
        if n is None:
            return items
        return items[:n]

    def elements(self):
        for key, count in self._data.items():
            i = 0
            while i < count:
                yield key
                i += 1

    def total(self):
        return sum(self._data.values())

    def __missing__(self, key):
        return 0

    def __repr__(self):
        return "Counter(" + repr(self._data) + ")"

    def __add__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for k, v in self._data.items():
            new = v + other._data.get(k, 0)
            if new > 0:
                result._data[k] = new
        for k, v in other._data.items():
            if k not in self._data and v > 0:
                result._data[k] = v
        return result

    def __sub__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for k, v in self._data.items():
            new = v - other._data.get(k, 0)
            if new > 0:
                result._data[k] = new
        return result

    def __or__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for k, v in self._data.items():
            other_v = other._data.get(k, 0)
            best = v if v > other_v else other_v
            if best > 0:
                result._data[k] = best
        for k, v in other._data.items():
            if k not in self._data and v > 0:
                result._data[k] = v
        return result

    def __and__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        result = Counter()
        for k, v in self._data.items():
            other_v = other._data.get(k, 0)
            best = v if v < other_v else other_v
            if best > 0:
                result._data[k] = best
        return result

    def __pos__(self):
        result = Counter()
        for k, v in self._data.items():
            if v > 0:
                result._data[k] = v
        return result

    def __neg__(self):
        result = Counter()
        for k, v in self._data.items():
            if v < 0:
                result._data[k] = -v
        return result


class ChainMap:
    """View multiple dicts as a single mapping."""

    def __init__(self, *maps):
        if not maps:
            maps = ({},)
        self.maps = list(maps)

    def __getitem__(self, key):
        for m in self.maps:
            if key in m:
                return m[key]
        raise KeyError(key)

    def __setitem__(self, key, value):
        self.maps[0][key] = value

    def __delitem__(self, key):
        if key in self.maps[0]:
            del self.maps[0][key]
        else:
            raise KeyError(key)

    def __contains__(self, key):
        for m in self.maps:
            if key in m:
                return True
        return False

    def __len__(self):
        seen = set()
        for m in self.maps:
            for k in m:
                seen.add(k)
        return len(seen)

    def __iter__(self):
        seen = set()
        for m in self.maps:
            for k in m:
                if k not in seen:
                    seen.add(k)
                    yield k

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def keys(self):
        return list(iter(self))

    def values(self):
        return [self[k] for k in self]

    def items(self):
        return [(k, self[k]) for k in self]

    def new_child(self, m=None):
        if m is None:
            m = {}
        return ChainMap(m, *self.maps)

    @property
    def parents(self):
        return ChainMap(*self.maps[1:])

    def __repr__(self):
        return "ChainMap(" + ", ".join(repr(m) for m in self.maps) + ")"


def namedtuple(typename, field_names, *, rename=False, defaults=None, module=None):
    """Return a new lightweight class with the given fields.

    The result mirrors CPython's ``namedtuple`` API surface — iteration,
    indexing, ``_asdict``, ``_replace``, and ``_fields`` — without
    inheriting from the built-in ``tuple`` type, which would require
    full MRO dispatch onto built-in types.
    """

    if isinstance(field_names, str):
        field_names = field_names.replace(",", " ").split()
    field_names = list(field_names)

    if rename:
        seen = set()
        for i, name in enumerate(field_names):
            if (
                not name.isidentifier()
                or name.startswith("_")
                or name in seen
            ):
                field_names[i] = "_" + str(i)
            seen.add(field_names[i])

    if defaults is not None:
        defaults = tuple(defaults)
        if len(defaults) > len(field_names):
            raise TypeError("got more defaults than field names")
        field_defaults = dict(
            zip(field_names[-len(defaults):], defaults)
        )
    else:
        field_defaults = {}

    class _NT:
        _fields = tuple(field_names)
        _field_defaults = field_defaults
        __match_args__ = tuple(field_names)

        @classmethod
        def _make(cls, iterable):
            return cls(*iterable)

        def __getnewargs__(self):
            return tuple(self._values)

        def __init__(self, *args, **kwargs):
            values = list(args)
            i = len(values)
            while i < len(field_names):
                name = field_names[i]
                if name in kwargs:
                    values.append(kwargs[name])
                elif name in field_defaults:
                    values.append(field_defaults[name])
                else:
                    raise TypeError(
                        "missing required argument: " + repr(name)
                    )
                i += 1
            if len(values) != len(field_names):
                raise TypeError("wrong number of arguments")
            self._values = tuple(values)
            for name, value in zip(field_names, values):
                setattr(self, name, value)

        def __iter__(self):
            return iter(self._values)

        def __getitem__(self, index):
            return self._values[index]

        def __len__(self):
            return len(self._values)

        def __eq__(self, other):
            if isinstance(other, _NT):
                return self._values == other._values
            if isinstance(other, tuple):
                return self._values == other
            return NotImplemented

        def _asdict(self):
            return dict(zip(field_names, self._values))

        def _replace(self, **changes):
            values = list(self._values)
            for i, name in enumerate(field_names):
                if name in changes:
                    values[i] = changes.pop(name)
            if changes:
                # Match CPython: leftover keys are reported as a TypeError
                # ("Got unexpected field names: [...]").
                raise TypeError(
                    "Got unexpected field names: " + repr(list(changes))
                )
            return type(self)(*values)

        def __replace__(self, **changes):
            return self._replace(**changes)

        def __repr__(self):
            parts = []
            for name, value in zip(field_names, self._values):
                parts.append(name + "=" + repr(value))
            return typename + "(" + ", ".join(parts) + ")"

    _NT.__name__ = typename
    _NT.__qualname__ = typename
    if module is not None:
        _NT.__module__ = module

    return _NT


# Pull in the abc-backed user wrappers last (see note near `__all__`).
from _collections_user import UserDict, UserList, UserString
