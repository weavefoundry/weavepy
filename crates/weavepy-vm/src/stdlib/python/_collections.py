"""WeavePy's `_collections` accelerator module.

CPython implements `deque`, `defaultdict`, `OrderedDict`, `_tuplegetter`
and `_count_elements` in C here; the verbatim `collections/__init__.py`
imports each inside `try/except ImportError` and falls back to its
pure-Python definitions when absent.

WeavePy supplies the two containers that have *no* pure-Python fallback
in the real module — `deque` and `defaultdict` — plus `_count_elements`,
an `OrderedDict` with the C implementation's observable semantics
(state-guarded iterators that pickle, gh-119004 mutation checks in
`__eq__`), and `_tuplegetter`. The collections fallback for the latter —
`property(_itemgetter(index))` — is observably different: a 3.13
property picks up `__name__` via `__set_name__`, so `pydoc` prints a
title line for namedtuple fields that CPython's C descriptor never has
(test_pydoc test_namedtuple_field_descriptor).
"""

__all__ = ["deque", "defaultdict", "OrderedDict", "_count_elements"]

# CPython's C `deque`/`defaultdict` expose `__class_getitem__` so PEP 585
# subscription (`deque[int]`) yields a `types.GenericAlias`. `types` only
# imports `sys`, so this is import-cycle safe from this low-level module.
from types import GenericAlias as _GenericAlias


# Per-`repr` recursion guard for `defaultdict.__repr__` (the moral
# equivalent of CPython's `Py_ReprEnter` on the defaultdict object).
_dd_repr_running = set()


def _count_elements(mapping, iterable):
    """Tally elements from the iterable (Counter's inner loop)."""
    mapping_get = mapping.get
    for elem in iterable:
        mapping[elem] = mapping_get(elem, 0) + 1


# Descriptor for a named tuple field: `obj[index]` carrying the field's
# docstring (CPython's C `_collections._tuplegetter`). No class
# docstring — `__doc__` must stay a slot so each instance carries its
# field's doc.
class _tuplegetter:
    __slots__ = ('index', '__doc__')

    def __init__(self, index, doc):
        self.index = index
        self.__doc__ = doc

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        try:
            return obj[self.index]
        except IndexError:
            raise AttributeError('tuple index out of range') from None

    def __set__(self, obj, value):
        raise AttributeError("can't set attribute")

    def __delete__(self, obj):
        raise AttributeError("can't delete attribute")

    def __reduce__(self):
        return (self.__class__, (self.index, self.__doc__))


class _defaultdict_factory_slot:
    # The C type's `default_factory` is a `tp_members` slot: a *data*
    # descriptor (`inspect.isdatadescriptor(defaultdict.default_factory)`
    # — test_inspect test_excluding_predicates), so it must own both
    # `__get__` and `__set__`. Instance storage lives under a private
    # instance-dict key (the C struct field's stand-in). Code like
    # `dataclasses._asdict_inner` probes `hasattr(type(obj),
    # 'default_factory')`, which this satisfies too.
    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return obj.__dict__.get('_weavepy_default_factory')

    def __set__(self, obj, value):
        obj.__dict__['_weavepy_default_factory'] = value

    def __delete__(self, obj):
        obj.__dict__['_weavepy_default_factory'] = None


class defaultdict(dict):
    """dict subclass that calls a factory function to supply missing values."""

    # The C type reports `collections`, not `_collections`.
    __module__ = "collections"

    default_factory = _defaultdict_factory_slot()

    def __init__(self, default_factory=None, /, *args, **kwds):
        if default_factory is not None and not callable(default_factory):
            raise TypeError("first argument must be callable or None")
        dict.__init__(self, *args, **kwds)
        self.default_factory = default_factory

    def __missing__(self, key):
        if self.default_factory is None:
            raise KeyError(key)
        # The factory runs *before* the insert and may itself populate
        # the key (re-entering `d[key]`); the first-inserted value wins
        # (CPython gh-91618 — test_factory_conflict_with_set_value).
        value = self.default_factory()
        return self.setdefault(key, value)

    def __repr__(self):
        # CPython's `defdict_repr` wraps the *factory* repr in
        # `Py_ReprEnter`: a factory whose repr reaches back into this
        # mapping (a bound method of self, or a factory that reprs the
        # dict — gh-145492) renders as `...` instead of recursing.
        key = id(self)
        if key in _dd_repr_running:
            factory_repr = "..."
        else:
            _dd_repr_running.add(key)
            try:
                factory_repr = repr(self.default_factory)
            finally:
                _dd_repr_running.discard(key)
        return f"{type(self).__name__}({factory_repr}, {dict.__repr__(self)})"

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

    # CPython's C deque has no `tp_dictoffset`: plain deques reject
    # attribute assignment, and a subclass may list '__dict__' in its
    # own `__slots__` (test_deque DequeWithSlots). It *does* set
    # `tp_weaklistoffset`, so weak references work (test_weakref).
    # `_head` is the count of consumed slots at the front of `_data`
    # (RFC 0077 WS6): `popleft` advances it in O(1) instead of shifting
    # the whole list, and the dead prefix is dropped once it reaches half
    # the list, so the queue-shaped `append`/`popleft` traffic asyncio's
    # ready queue and `queue.Queue` generate is amortized O(1) per
    # operation. Every other method flattens through `_flat()` first.
    __slots__ = ("_data", "_maxlen", "_state", "_head", "__weakref__")

    # CPython's C deque carries Py_TPFLAGS_SEQUENCE, so `case [..]:`
    # patterns match deques (PEP 634). WeavePy's VM reads the flag off
    # this private marker (the same key ABCMeta stows __abc_tpflags__
    # under).
    _abc_collection_flags = 1 << 5  # Py_TPFLAGS_SEQUENCE

    def __init__(self, iterable=(), maxlen=None):
        if maxlen is not None:
            if not isinstance(maxlen, int):
                raise TypeError("an integer is required")
            if maxlen < 0:
                raise ValueError("maxlen must be non-negative")
        self._data = []
        self._head = 0
        self._maxlen = maxlen
        # Mutation counter (CPython's `deque->state`): live iterators
        # compare against their snapshot and raise "deque mutated during
        # iteration" on mismatch.
        self._state = 0
        self.extend(iterable)

    @property
    def maxlen(self):
        return self._maxlen

    def _flat(self):
        """The backing list with the consumed prefix removed."""
        h = self._head
        if h:
            del self._data[:h]
            self._head = 0
        return self._data

    def append(self, x):
        # CPython's method descriptor rejects unbound calls with a
        # foreign receiver (`deque.append(thing, x)` — gh-92063).
        if not isinstance(self, deque):
            raise TypeError(
                "descriptor 'append' for 'collections.deque' objects "
                "doesn't apply to a '%s' object" % type(self).__name__
            )
        self._state += 1
        self._data.append(x)
        if self._maxlen is not None and len(self._data) - self._head > self._maxlen:
            self.popleft()

    def appendleft(self, x):
        # Fill the consumed prefix from the right; when there is none,
        # open a block of slack proportional to the size so a run of
        # `appendleft` calls is amortized O(1) like CPython's block
        # deque (rather than `list.insert(0, x)`'s O(n) shift).
        self._state += 1
        data = self._data
        h = self._head
        if h == 0:
            h = max(8, len(data) // 2)
            data[0:0] = [None] * h
        h -= 1
        data[h] = x
        self._head = h
        if self._maxlen is not None and len(data) - h > self._maxlen:
            data.pop()

    def pop(self):
        data = self._data
        if len(data) <= self._head:
            raise IndexError("pop from an empty deque")
        self._state += 1
        x = data.pop()
        if len(data) == self._head and self._head:
            del data[:]
            self._head = 0
        return x

    def popleft(self):
        data = self._data
        h = self._head
        if h >= len(data):
            raise IndexError("pop from an empty deque")
        self._state += 1
        x = data[h]
        data[h] = None
        h += 1
        if h >= len(data):
            del data[:]
            h = 0
        elif h >= 32 and h * 2 >= len(data):
            del data[:h]
            h = 0
        self._head = h
        return x

    def extend(self, iterable):
        # `d.extend(d)` iterates a snapshot (CPython special-cases
        # self-extension the same way).
        if iterable is self:
            iterable = list(self._flat())
        for item in iterable:
            self.append(item)

    def extendleft(self, iterable):
        if iterable is self:
            iterable = list(self._flat())
        for item in iterable:
            self.appendleft(item)

    def rotate(self, n=1):
        data = self._flat()
        if not data:
            return
        size = len(data)
        n = n % size
        if n == 0:
            return
        self._state += 1
        self._data = data[-n:] + data[:-n]

    def clear(self):
        self._state += 1
        del self._data[:]
        self._head = 0

    def copy(self):
        return type(self)(self._flat(), self._maxlen)

    __copy__ = copy

    def count(self, value):
        # CPython `deque_count`: per-comparison mutation trip-wire — an
        # `__eq__` that mutates the deque raises RuntimeError.
        data = self._flat()
        state = self._state
        n = len(data)
        result = 0
        i = 0
        while i < n:
            item = data[i]
            if item is value or item == value:
                result += 1
            if self._state != state:
                raise RuntimeError("deque mutated during iteration")
            i += 1
        return result

    def index(self, value, start=0, stop=None):
        data = self._flat()
        state = self._state
        if stop is None:
            stop = len(data)
        n = len(data)
        if start < 0:
            start = max(0, start + n)
        if stop < 0:
            stop += n
        for i in range(start, min(stop, n)):
            item = data[i]
            hit = item is value or item == value
            if self._state != state:
                raise RuntimeError("deque mutated during iteration")
            if hit:
                return i
        raise ValueError(f"{value!r} is not in deque")

    def insert(self, i, x):
        if self._maxlen is not None and len(self) >= self._maxlen:
            raise IndexError("deque already at its maximum size")
        self._state += 1
        self._flat().insert(i, x)

    def remove(self, value):
        # CPython `deque_remove`: a size change caused by a comparison's
        # side effects is an IndexError, distinct from the iteration guard.
        data = self._flat()
        n = len(data)
        i = 0
        while i < n:
            item = data[i]
            hit = item is value or item == value
            if len(data) != n:
                raise IndexError("deque mutated during remove().")
            if hit:
                self._state += 1
                del data[i]
                return
            i += 1
        raise ValueError("deque.remove(x): x not in deque")

    def reverse(self):
        self._state += 1
        self._flat().reverse()

    def __len__(self):
        return len(self._data) - self._head

    def __bool__(self):
        return len(self._data) > self._head

    def __iter__(self):
        return _deque_iterator(self)

    def __reversed__(self):
        return _deque_reverse_iterator(self)

    def __contains__(self, x):
        # CPython `deque_contains`: mutation during a comparison raises.
        data = self._flat()
        state = self._state
        i = 0
        while i < len(data):
            item = data[i]
            # Element on the left (CPython `PyObject_RichCompareBool(item,
            # v, Py_EQ)`): the *item's* __eq__ gets first shot.
            hit = item is x or item == x
            if self._state != state:
                raise RuntimeError("deque mutated during iteration")
            if hit:
                return True
            i += 1
        return False

    def _slot(self, idx):
        # Translate a logical index into a `_data` slot, honoring the
        # consumed prefix without flattening (`d[0]`/`d[-1]` peeks stay
        # O(1) on a queue that is being drained from the left).
        try:
            i = idx.__index__()
        except AttributeError:
            raise TypeError(
                "sequence index must be integer, not '%s'" % type(idx).__name__
            ) from None
        n = len(self._data) - self._head
        if i < 0:
            i += n
        if i < 0 or i >= n:
            raise IndexError("deque index out of range")
        return self._head + i

    def __getitem__(self, idx):
        return self._data[self._slot(idx)]

    def __setitem__(self, idx, value):
        # In-place replacement does NOT invalidate live iterators (CPython's
        # `deque_ass_item` leaves `state` alone; test_deque
        # test_iterator_pickle mutates through `d[i] = x` mid-iteration).
        self._data[self._slot(idx)] = value

    def __delitem__(self, idx):
        self._state += 1
        del self._flat()[idx]

    def __add__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        new = self.copy()
        new.extend(other._flat())
        return new

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        if not isinstance(n, int):
            return NotImplemented
        return type(self)(self._flat() * n, self._maxlen)

    __rmul__ = __mul__

    def __imul__(self, n):
        self._state += 1
        data = self._flat()
        data *= n
        if self._maxlen is not None and len(data) > self._maxlen:
            del data[: len(data) - self._maxlen]
        return self

    def _cmp_seq(self, other):
        return other._flat() if isinstance(other, deque) else NotImplemented

    def __eq__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() == other._flat()

    def __ne__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() != other._flat()

    def __lt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() < other._flat()

    def __le__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() <= other._flat()

    def __gt__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() > other._flat()

    def __ge__(self, other):
        if not isinstance(other, deque):
            return NotImplemented
        return self._flat() >= other._flat()

    __hash__ = None

    __class_getitem__ = classmethod(_GenericAlias)

    def __reduce__(self):
        # CPython `deque_reduce`: `(type, () | ((), maxlen), state, iter(d))`.
        # The elements travel as *list items* (applied by `append` after the
        # object is memoized), so a self-referential deque round-trips
        # (test_deque.test_pickle_recursive). The internal `_data`/`_state`
        # slots must stay out of `state` or they'd double-apply the items.
        # The base deque has no instance dict (C deque tp_dictoffset == 0);
        # only a subclass that re-adds one contributes dict state.
        dictstate = {
            k: v
            for k, v in getattr(self, "__dict__", {}).items()
            if k not in ("_data", "_state", "_maxlen", "_head")
        } or None
        # Mirror `object.__getstate__`: subclass __slots__ values travel in
        # the second half of a (dict, slots) pair. The deque-internal slots
        # stay out — the elements travel as list items instead.
        slotstate = {}
        for klass in type(self).__mro__:
            if klass is deque:
                continue
            slots = klass.__dict__.get("__slots__", ())
            if isinstance(slots, str):
                slots = (slots,)
            for name in slots:
                if name in ("__dict__", "__weakref__"):
                    continue
                try:
                    slotstate[name] = getattr(self, name)
                except AttributeError:
                    pass
        state = (dictstate, slotstate) if slotstate else dictstate
        if self._maxlen is None:
            args = ()
        else:
            args = ((), self._maxlen)
        return type(self), args, state, iter(self)

    def __repr__(self):
        # Recursion guard (CPython's `Py_ReprEnter`): a self-containing
        # deque renders the inner occurrence as `[...]`.
        k = id(self)
        if k in _repr_running:
            return "[...]"
        _repr_running.add(k)
        try:
            data = self._flat()
            if self._maxlen is None:
                return f"{type(self).__name__}({data!r})"
            return f"{type(self).__name__}({data!r}, maxlen={self._maxlen})"
        finally:
            _repr_running.discard(k)


_repr_running = set()


class _deque_iterator:
    """Iterator over a live deque (CPython's `_collections._deque_iterator`).

    Holds the deque itself and a cursor; any deque mutation after creation
    bumps `deque._state` and the next `__next__` raises `RuntimeError`
    (sticky — the iterator is dead afterwards, `__length_hint__` reports 0).
    """

    def __init__(self, deq, index=0):
        if not isinstance(deq, deque):
            raise TypeError("deque expected")
        self._deq = deq
        self._index = index
        self._deq_state = deq._state

    def __iter__(self):
        return self

    def __next__(self):
        deq = self._deq
        if deq is None:
            raise StopIteration
        if deq._state != self._deq_state:
            self._deq = None
            raise RuntimeError("deque mutated during iteration")
        i = self._index
        if i >= len(deq._data) - deq._head:
            self._deq = None
            raise StopIteration
        self._index = i + 1
        return deq._data[deq._head + i]

    def __length_hint__(self):
        deq = self._deq
        if deq is None or deq._state != self._deq_state:
            return 0
        return len(deq) - self._index

    def __reduce__(self):
        deq = self._deq
        if deq is None:
            return type(self), (deque(),)
        return type(self), (deq, self._index)


class _OrderedDictNode:
    """Doubly-linked-list node backing `OrderedDict`'s insertion order
    (CPython's `_ODictNode`)."""

    __slots__ = ("prev", "next", "key")


class _OrderedDictIter:
    """State-guarded iterator over an OrderedDict's linked list
    (CPython's `odict_iterator`). `kind` selects keys (0), values (1)
    or items (2); mutating the od between `__next__` calls raises
    RuntimeError, exactly like the C iterator's `od_state` check."""

    def __init__(self, od, kind, reverse):
        self._od = od
        self._kind = kind
        self._reverse = reverse
        root = od._OrderedDict__root
        self._node = root.prev if reverse else root.next
        self._state = od._OrderedDict__state
        self._remaining = dict.__len__(od)

    def __iter__(self):
        return self

    def __next__(self):
        od = self._od
        if od is None:
            raise StopIteration
        if od._OrderedDict__state != self._state:
            self._od = None
            raise RuntimeError("OrderedDict mutated during iteration")
        node = self._node
        if node is od._OrderedDict__root:
            self._od = None
            raise StopIteration
        self._node = node.prev if self._reverse else node.next
        self._remaining -= 1
        key = node.key
        if self._kind == 0:
            return key
        value = dict.__getitem__(od, key)
        if self._kind == 1:
            return value
        return (key, value)

    def __length_hint__(self):
        od = self._od
        if od is None or od._OrderedDict__state != self._state:
            return 0
        return self._remaining

    def __reduce__(self):
        # CPython's odict iterators pickle as a plain `iter` over the
        # *remaining* elements (di_size snapshot walk), leaving the
        # live iterator undisturbed (test_ordered_dict
        # test_iterators_pickled).
        remaining = []
        od = self._od
        if od is not None and od._OrderedDict__state == self._state:
            node = self._node
            root = od._OrderedDict__root
            while node is not root:
                key = node.key
                if self._kind == 0:
                    remaining.append(key)
                elif self._kind == 1:
                    remaining.append(dict.__getitem__(od, key))
                else:
                    remaining.append((key, dict.__getitem__(od, key)))
                node = node.prev if self._reverse else node.next
        return iter, (remaining,)


_odict_views = None


def _get_odict_views():
    """Lazily build the KeysView/ValuesView/ItemsView subclasses the
    view methods hand out (CPython's odict_keys/odict_values/
    odict_items). Deferred so `_collections` never imports
    `_collections_abc` at module-exec time."""
    global _odict_views
    if _odict_views is None:
        from _collections_abc import ItemsView, KeysView, ValuesView

        class odict_keys(KeysView):
            def __iter__(self):
                return _OrderedDictIter(self._mapping, 0, False)

            def __reversed__(self):
                return _OrderedDictIter(self._mapping, 0, True)

        class odict_values(ValuesView):
            def __iter__(self):
                return _OrderedDictIter(self._mapping, 1, False)

            def __reversed__(self):
                return _OrderedDictIter(self._mapping, 1, True)

        class odict_items(ItemsView):
            def __iter__(self):
                return _OrderedDictIter(self._mapping, 2, False)

            def __reversed__(self):
                return _OrderedDictIter(self._mapping, 2, True)

        _odict_views = (odict_keys, odict_values, odict_items)
    return _odict_views


_odict_repr_running = set()


class OrderedDict(dict):
    'Dictionary that remembers insertion order'

    # `pickle` resolves the class through `collections` (which re-exports
    # this one), and the C implementation reports that module too.
    __module__ = "collections"

    # The linked list mirrors CPython's odict: each key owns a node, the
    # root sentinel closes the circle, and `__state` counts structural
    # changes (insert / delete / move / clear) so iterators and `__eq__`
    # can detect concurrent mutation (gh-119004). All state is created in
    # `__new__` — a subclass overriding `__init__` without calling up
    # still gets a consistent od (test_overridden_init).

    def __new__(cls, /, *args, **kwds):
        self = dict.__new__(cls)
        root = _OrderedDictNode()
        root.prev = root.next = root
        root.key = None
        self.__root = root
        self.__map = {}
        self.__state = 0
        return self

    def __init__(self, other=(), /, **kwds):
        self.__update(other, kwds)

    def __update(self, other, kwds):
        # CPython `mutablemapping_update`: dispatch through
        # `PyObject_SetItem` so subclass `__setitem__` overrides apply.
        if isinstance(other, dict):
            for key in other:
                self[key] = other[key]
        elif hasattr(other, "keys"):
            for key in other.keys():
                self[key] = other[key]
        else:
            for key, value in other:
                self[key] = value
        for key, value in kwds.items():
            self[key] = value

    def update(self, other=(), /, **kwds):
        self.__update(other, kwds)

    def __setitem__(self, key, value):
        if key not in self.__map:
            root = self.__root
            last = root.prev
            node = _OrderedDictNode()
            node.prev, node.next, node.key = last, root, key
            dict.__setitem__(self, key, value)
            last.next = node
            root.prev = node
            self.__map[key] = node
            self.__state += 1
        else:
            # Overwriting a value leaves the order (and od_state)
            # untouched — live iterators stay valid, as with dict.
            dict.__setitem__(self, key, value)

    def __delitem__(self, key):
        dict.__delitem__(self, key)
        self.__unlink(key)

    def __unlink(self, key):
        node = self.__map.pop(key)
        node.prev.next = node.next
        node.next.prev = node.prev
        node.prev = node.next = None
        self.__state += 1

    def __iter__(self):
        return _OrderedDictIter(self, 0, False)

    def __reversed__(self):
        return _OrderedDictIter(self, 0, True)

    def clear(self):
        dict.clear(self)
        self.__map.clear()
        root = self.__root
        root.prev = root.next = root
        self.__state += 1

    def popitem(self, last=True):
        '''Remove and return a (key, value) pair from the dictionary.

        Pairs are returned in LIFO order if last is true or FIFO order if false.
        '''
        if not dict.__len__(self):
            raise KeyError('dictionary is empty')
        root = self.__root
        node = root.prev if last else root.next
        key = node.key
        value = dict.pop(self, key)
        self.__unlink(key)
        return key, value

    def move_to_end(self, key, last=True):
        '''Move an existing element to the end (or beginning if last is false).

        Raise KeyError if the element does not exist.
        '''
        node = self.__map[key]
        node.prev.next = node.next
        node.next.prev = node.prev
        root = self.__root
        if last:
            prev = root.prev
            node.prev, node.next = prev, root
            prev.next = node
            root.prev = node
        else:
            nxt = root.next
            node.prev, node.next = root, nxt
            root.next = node
            nxt.prev = node
        self.__state += 1

    def keys(self):
        "D.keys() -> a set-like object providing a view on D's keys"
        return _get_odict_views()[0](self)

    def values(self):
        "D.values() -> an object providing a view on D's values"
        return _get_odict_views()[1](self)

    def items(self):
        "D.items() -> a set-like object providing a view on D's items"
        return _get_odict_views()[2](self)

    __marker = object()

    def pop(self, key, default=__marker):
        '''od.pop(k[,d]) -> v, remove specified key and return the corresponding
        value.  If key is not found, d is returned if given, otherwise KeyError
        is raised.

        '''
        marker = self.__marker
        result = dict.pop(self, key, marker)
        if result is not marker:
            self.__unlink(key)
            return result
        if default is marker:
            raise KeyError(key)
        return default

    def setdefault(self, key, default=None):
        '''Insert key with a value of default if key is not in the dictionary.

        Return the value for key if key is in the dictionary, else default.
        '''
        if key in self:
            return self[key]
        self[key] = default
        return default

    def __repr__(self):
        'od.__repr__() <==> repr(od)'
        # reprlib.recursive_repr by hand: `od['x'] = od` renders as
        # `OrderedDict({... , 'x': ...})`.
        marker = id(self)
        if marker in _odict_repr_running:
            return '...'
        _odict_repr_running.add(marker)
        try:
            if not dict.__len__(self):
                return '%s()' % (self.__class__.__name__,)
            return '%s(%r)' % (self.__class__.__name__, dict(self.items()))
        finally:
            _odict_repr_running.discard(marker)

    def __reduce__(self):
        'Return state information for pickling'
        state = self.__getstate__()
        if state:
            if isinstance(state, tuple):
                state, slots = state
            else:
                slots = {}
            state = state.copy()
            slots = slots.copy()
            for k in vars(OrderedDict()):
                state.pop(k, None)
                slots.pop(k, None)
            if slots:
                state = state, slots
            else:
                state = state or None
        return self.__class__, (), state, None, iter(self.items())

    def copy(self):
        'od.copy() -> a shallow copy of od'
        return self.__class__(self)

    @classmethod
    def fromkeys(cls, iterable, value=None):
        '''Create a new ordered dictionary with keys from iterable and values set to value.
        '''
        self = cls()
        for key in iterable:
            self[key] = value
        return self

    def __eq__(self, other):
        '''od.__eq__(y) <==> od==y.  Comparison to another OD is order-sensitive
        while comparison to a regular mapping is order-insensitive.

        '''
        if not isinstance(other, dict):
            return NotImplemented
        eq = dict.__eq__(self, other)
        if not isinstance(other, OrderedDict) or eq is not True:
            return eq
        # CPython `_odict_keys_equal`: after the dict-level comparison, an
        # order-sensitive walk over both linked lists, snapshotting each
        # od's state up front and re-checking it after every key
        # comparison — a key `__eq__` that mutates either od raises
        # RuntimeError (gh-119004).
        state1 = self.__state
        state2 = other._OrderedDict__state
        root1 = self.__root
        root2 = other._OrderedDict__root
        node1 = root1.next
        node2 = root2.next
        while True:
            if node1 is root1 and node2 is root2:
                return True
            if node1 is root1 or node2 is root2:
                return False
            k1 = node1.key
            k2 = node2.key
            keys_eq = k1 is k2 or bool(k1 == k2)
            if self.__state != state1 or other._OrderedDict__state != state2:
                raise RuntimeError("OrderedDict mutated during iteration")
            if not keys_eq:
                return False
            node1 = node1.next
            node2 = node2.next

    def __ne__(self, other):
        'od.__ne__(y) <==> od!=y'
        # Without this, `!=` on two ods would resolve dict's native
        # (order-insensitive) comparison; C odict routes NE through the
        # same order-sensitive tp_richcompare as EQ.
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return NotImplemented
        return not eq

    def __sizeof__(self):
        # dict payload + one linked-list node per key (plus the root)
        # + the key->node map: mirrors the C implementation reporting
        # strictly more than an equal plain dict (test_sizeof).
        n = dict.__len__(self) + 1
        return dict.__sizeof__(self) + n * 32 + 64

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        new = self.__class__(self)
        new.update(other)
        return new

    def __ror__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        new = self.__class__(other)
        new.update(self)
        return new


class _deque_reverse_iterator:
    """Reverse iterator over a live deque
    (CPython's `_collections._deque_reverse_iterator`)."""

    def __init__(self, deq, index=0):
        if not isinstance(deq, deque):
            raise TypeError("deque expected")
        self._deq = deq
        # `index` counts consumed items, mirroring the forward iterator's
        # constructor/`__reduce__` contract.
        self._index = index
        self._deq_state = deq._state

    def __iter__(self):
        return self

    def __next__(self):
        deq = self._deq
        if deq is None:
            raise StopIteration
        if deq._state != self._deq_state:
            self._deq = None
            raise RuntimeError("deque mutated during iteration")
        i = len(deq._data) - 1 - self._index
        if i < deq._head:
            self._deq = None
            raise StopIteration
        self._index += 1
        return deq._data[i]

    def __length_hint__(self):
        deq = self._deq
        if deq is None or deq._state != self._deq_state:
            return 0
        return len(deq) - self._index

    def __reduce__(self):
        deq = self._deq
        if deq is None:
            return type(self), (deque(),)
        return type(self), (deq, self._index)
