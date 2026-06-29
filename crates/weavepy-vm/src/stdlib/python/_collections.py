"""WeavePy's `_collections` accelerator module.

CPython implements `deque`, `defaultdict`, `OrderedDict`, `_tuplegetter`
and `_count_elements` in C here; the verbatim `collections/__init__.py`
imports each inside `try/except ImportError` and falls back to its
pure-Python definitions when absent.

WeavePy supplies the two containers that have *no* pure-Python fallback
in the real module — `deque` and `defaultdict` — plus `_count_elements`.
`OrderedDict` and `_tuplegetter` are intentionally omitted so the
reference pure-Python implementations run instead.
"""

__all__ = ["deque", "defaultdict", "_count_elements"]

# CPython's C `deque`/`defaultdict` expose `__class_getitem__` so PEP 585
# subscription (`deque[int]`) yields a `types.GenericAlias`. `types` only
# imports `sys`, so this is import-cycle safe from this low-level module.
from types import GenericAlias as _GenericAlias


def _count_elements(mapping, iterable):
    """Tally elements from the iterable (Counter's inner loop)."""
    mapping_get = mapping.get
    for elem in iterable:
        mapping[elem] = mapping_get(elem, 0) + 1


class defaultdict(dict):
    """dict subclass that calls a factory function to supply missing values."""

    # The C type reports `collections`, not `_collections`.
    __module__ = "collections"

    # Class-level default mirrors the C type's member descriptor; code like
    # `dataclasses._asdict_inner` probes `hasattr(type(obj),
    # 'default_factory')` to recognize defaultdict-shaped mappings.
    default_factory = None

    def __init__(self, default_factory=None, /, *args, **kwds):
        if default_factory is not None and not callable(default_factory):
            raise TypeError("first argument must be callable or None")
        dict.__init__(self, *args, **kwds)
        self.default_factory = default_factory

    def __missing__(self, key):
        if self.default_factory is None:
            raise KeyError(key)
        self[key] = value = self.default_factory()
        return value

    def __repr__(self):
        return (
            f"{type(self).__name__}({self.default_factory!r}, {dict.__repr__(self)})"
        )

    def copy(self):
        return type(self)(self.default_factory, self)

    __copy__ = copy

    def __reduce__(self):
        if self.default_factory is None:
            args = ()
        else:
            args = (self.default_factory,)
        return type(self), args, None, None, iter(self.items())

    def __or__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        new = self.copy()
        new.update(other)
        return new

    def __ror__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        new = type(self)(self.default_factory, other)
        new.update(self)
        return new

    __class_getitem__ = classmethod(_GenericAlias)


class deque:
    """list-like container with fast appends and pops on either end.

    Pure-Python stand-in for CPython's doubly-linked-block C deque; it
    keeps the public API (append/appendleft, pop/popleft, maxlen
    discipline, rotate, +, *, comparison, …) over a plain list.
    """

    # The C type reports `collections`, not `_collections` (annotation
    # formatting and pickling both key off this).
    __module__ = "collections"

    def __init__(self, iterable=(), maxlen=None):
        if maxlen is not None:
            if not isinstance(maxlen, int):
                raise TypeError("an integer is required")
            if maxlen < 0:
                raise ValueError("maxlen must be non-negative")
        self._data = []
        self._maxlen = maxlen
        self.extend(iterable)

    @property
    def maxlen(self):
        return self._maxlen

    def append(self, x):
        # CPython's method descriptor rejects unbound calls with a
        # foreign receiver (`deque.append(thing, x)` — gh-92063).
        if not isinstance(self, deque):
            raise TypeError(
                "descriptor 'append' for 'collections.deque' objects "
                "doesn't apply to a '%s' object" % type(self).__name__
            )
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
        del self._data[:]

    def copy(self):
        return type(self)(self._data, self._maxlen)

    __copy__ = copy

    def count(self, value):
        return sum(1 for item in self._data if item == value)

    def index(self, value, start=0, stop=None):
        if stop is None:
            stop = len(self._data)
        n = len(self._data)
        if start < 0:
            start = max(0, start + n)
        if stop < 0:
            stop += n
        for i in range(start, min(stop, n)):
            if self._data[i] == value:
                return i
        raise ValueError(f"{value!r} is not in deque")

    def insert(self, i, x):
        if self._maxlen is not None and len(self._data) >= self._maxlen:
            raise IndexError("deque already at its maximum size")
        self._data.insert(i, x)

    def remove(self, value):
        for i, item in enumerate(self._data):
            if item == value:
                del self._data[i]
                return
        raise ValueError("deque.remove(x): x not in deque")

    def reverse(self):
        self._data.reverse()

    def __len__(self):
        return len(self._data)

    def __bool__(self):
        return bool(self._data)

    def __iter__(self):
        return iter(self._data)

    def __reversed__(self):
        return reversed(self._data)

    def __contains__(self, x):
        return x in self._data

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            raise TypeError("sequence index must be integer, not 'slice'")
        return self._data[idx]

    def __setitem__(self, idx, value):
        self._data[idx] = value

    def __delitem__(self, idx):
        del self._data[idx]

    def __add__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        new = self.copy()
        new.extend(other._data)
        return new

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        if not isinstance(n, int):
            return NotImplemented
        return type(self)(self._data * n, self._maxlen)

    __rmul__ = __mul__

    def __imul__(self, n):
        self._data *= n
        if self._maxlen is not None and len(self._data) > self._maxlen:
            del self._data[: len(self._data) - self._maxlen]
        return self

    def _cmp_seq(self, other):
        return other._data if isinstance(other, deque) else NotImplemented

    def __eq__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data == other._data

    def __ne__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data != other._data

    def __lt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data < other._data

    def __le__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data <= other._data

    def __gt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data > other._data

    def __ge__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._data >= other._data

    __hash__ = None

    __class_getitem__ = classmethod(_GenericAlias)

    def __reduce__(self):
        return type(self), (list(self._data), self._maxlen)

    def __repr__(self):
        if self._maxlen is None:
            return f"{type(self).__name__}({self._data!r})"
        return f"{type(self).__name__}({self._data!r}, maxlen={self._maxlen})"
