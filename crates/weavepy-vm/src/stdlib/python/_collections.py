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
# The OrderedDict views subclass the ABC views (as CPython's subclass
# `dict_keys`, …); `os` has already imported `_collections_abc` by the
# time anything imports this module.
from _collections_abc import (
    ItemsView as _ItemsView,
    KeysView as _KeysView,
    ValuesView as _ValuesView,
)
from _weave_collections import iterator_index as _deque_iterator_index
from _weave_collections import (
    install_tuplegetter as _install_tuplegetter,
    tuplegetter_get as _tuplegetter_get,
    tuplegetter_index as _tuplegetter_index,
    tuplegetter_init as _tuplegetter_init,
)
# `namedtuple()` registers each generated `__new__` here, so the
# interpreter builds `NT(a, b, c)` as the tuple directly, and each generated
# `_make`, whose common cases (and `_replace`'s) run natively.
from _weave_collections import (
    namedtuple_make as _namedtuple_make,
    namedtuple_register as _namedtuple_register,
    namedtuple_register_make as _namedtuple_register_make,
    namedtuple_replace as _namedtuple_replace,
)
# `ChainMap`'s lookups (collections.py adopts them).
from _weave_collections import (
    chainmap_contains as _chainmap_contains,
    chainmap_getitem as _chainmap_getitem,
)
from _weave_collections import (
    count_elements as _count_elements_native,
    dd_init as _dd_init,
    dd_missing as _dd_missing,
    install_defaultdict as _install_defaultdict,
)
# The native `OrderedDict` operations (`stdlib/collections_odict.rs`).
from _weave_collections import (
    install_odict as _install_odict,
    od_clear as _od_clear,
    od_delitem as _od_delitem,
    od_iter as _od_iter,
    od_keys_equal as _od_keys_equal,
    od_move_to_end as _od_move_to_end,
    od_pop as _od_pop,
    od_popitem as _od_popitem,
    od_reversed as _od_reversed,
    od_setdefault as _od_setdefault,
    od_setitem as _od_setitem,
    od_update_fast as _od_update_fast,
    od_items as _od_items,
    od_keys as _od_keys,
    od_values as _od_values,
    odv_items_iter as _odv_items_iter,
    odv_items_reversed as _odv_items_reversed,
    odv_keys_iter as _odv_keys_iter,
    odv_keys_reversed as _odv_keys_reversed,
    odv_values_iter as _odv_values_iter,
    odv_values_reversed as _odv_values_reversed,
)


# Per-`repr` recursion guard for `defaultdict.__repr__` (the moral
# equivalent of CPython's `Py_ReprEnter` on the defaultdict object).
_dd_repr_running = set()


def _count_elements(mapping, iterable):
    """Tally elements from the iterable (Counter's inner loop)."""
    # A dict (or a subclass keeping `dict.get` and `dict.__setitem__`,
    # like `Counter`) is tallied natively, as CPython's C helper does.
    if _count_elements_native(mapping, iterable):
        return
    mapping_get = mapping.get
    for elem in iterable:
        mapping[elem] = mapping_get(elem, 0) + 1


# Descriptor for a named tuple field: `tuple.__getitem__(obj, index)`
# carrying the field's docstring (CPython's C `_collections._tuplegetter`).
# No class docstring — `__doc__` must stay a slot so each instance carries
# its field's doc. The index lives in native storage Python can't rebind
# (as in the C struct), and the class is marked so the interpreter's
# attribute paths read a field as a plain tuple index.
class _tuplegetter:
    __slots__ = ('__doc__',)

    def __new__(cls, index, doc):
        self = object.__new__(cls)
        _tuplegetter_init(self, index)
        self.__doc__ = doc
        return self

    __get__ = _tuplegetter_get

    def __set__(self, obj, value):
        raise AttributeError("can't set attribute")

    def __delete__(self, obj):
        raise AttributeError("can't delete attribute")

    def __reduce__(self):
        return (self.__class__, (_tuplegetter_index(self), self.__doc__))


_install_tuplegetter(_tuplegetter)


class defaultdict(dict):
    """dict subclass that calls a factory function to supply missing values."""

    # The C type reports `collections`, not `_collections`.
    __module__ = "collections"

    # The C type's `default_factory` is a `tp_members` slot: a member
    # descriptor that reads `None` when unset (`install_defaultdict`
    # finishes it), and the instances carry no `__dict__` and no weak
    # references.
    __slots__ = ("default_factory",)

    # Native (CPython's `defdict_init`): the factory is checked and set,
    # then the remaining arguments initialize the dict.
    __init__ = _dd_init

    # Native (CPython's `defdict_missing`): the factory's value, stored with
    # `dict.setdefault`, so a factory that fills the key itself keeps the
    # first value (gh-91618 — test_factory_conflict_with_set_value).
    __missing__ = _dd_missing

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


_install_defaultdict(defaultdict)


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

    # The four end operations are native builtins
    # (`stdlib/collections_native.rs`): CPython documents them as
    # thread-safe, and a Python method body is not atomic here (the GIL
    # is handed off at checkpoints inside it; `-X gil=0` runs it
    # concurrently). Each runs to completion under the backing list's
    # cell borrow, so `SimpleQueue.get` racing `put`, or asyncio's
    # `call_soon_threadsafe` racing the loop's `popleft`, can't tear the
    # head index. The unbound form keeps gh-92063's foreign-receiver
    # TypeError. Being builtins also makes `d.append` a
    # `builtin_function_or_method`, like the C accelerator's.
    from _weave_collections import (
        append, appendleft, pop, popleft, rotate,
        __len__, __bool__, __getitem__, __iter__, __reversed__,
    )

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

    __slots__ = ("_deq", "_index", "_deq_state")

    def __init__(self, deq, index=0):
        if not isinstance(deq, deque):
            raise TypeError("deque expected")
        self._deq = deq
        self._index = _deque_iterator_index(deq, index)
        self._deq_state = deq._state

    def __iter__(self):
        return self

    from _weave_collections import iterator_next as __next__

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


# Per-view `repr` recursion guard (CPython's `Py_ReprEnter` in
# `dictview_repr`): `od[k] = od.values()` renders the inner view as `...`.
_odict_view_repr_running = set()


def _odict_view_repr(view):
    key = id(view)
    if key in _odict_view_repr_running:
        return "..."
    _odict_view_repr_running.add(key)
    try:
        return f"{type(view).__name__}({list(view)!r})"
    finally:
        _odict_view_repr_running.discard(key)


# The views `OrderedDict.keys()`/`values()`/`items()` hand out (CPython's
# odict_keys/odict_values/odict_items): iterating one walks the order.
class odict_keys(_KeysView):
    __module__ = "builtins"
    __repr__ = _odict_view_repr
    __iter__ = _odv_keys_iter
    __reversed__ = _odv_keys_reversed


class odict_values(_ValuesView):
    __module__ = "builtins"
    __repr__ = _odict_view_repr
    __iter__ = _odv_values_iter
    __reversed__ = _odv_values_reversed


class odict_items(_ItemsView):
    __module__ = "builtins"
    __repr__ = _odict_view_repr
    __iter__ = _odv_items_iter
    __reversed__ = _odv_items_reversed


_odict_repr_running = set()


class OrderedDict(dict):
    'Dictionary that remembers insertion order'

    # `pickle` resolves the class through `collections` (which re-exports
    # this one), and the C implementation reports that module too.
    __module__ = "collections"

    # The dict payload holds the mapping; the order is a native keys-only
    # dict beside it (`stdlib/collections_odict.rs`), created on first use
    # (so a subclass overriding `__init__` without calling up still gets a
    # consistent od — test_overridden_init). `od[key]` reads, `len`, `in`
    # and the plain dict methods are the dict's own; the methods below
    # keep the order, as CPython's C `odict` does around its node list.
    __setitem__ = _od_setitem
    __delitem__ = _od_delitem
    __iter__ = _od_iter
    __reversed__ = _od_reversed
    move_to_end = _od_move_to_end
    popitem = _od_popitem
    pop = _od_pop
    clear = _od_clear

    def __init__(self, other=(), /, **kwds):
        self.__update(other, kwds)

    def __update(self, other, kwds):
        # CPython `mutablemapping_update`: dispatch through
        # `PyObject_SetItem` so subclass `__setitem__` overrides apply. A
        # class keeping the native `__setitem__` takes the common sources
        # natively.
        if type(self).__setitem__ is _od_setitem:
            if not _od_update_fast(self, other):
                self.__update_from(other)
            if kwds:
                _od_update_fast(self, kwds)
            return
        self.__update_from(other)
        for key, value in kwds.items():
            self[key] = value

    def __update_from(self, other):
        if type(other) is dict:
            for key, value in list(other.items()):
                self[key] = value
            return
        keys = getattr(other, "keys", None)
        if keys is not None:
            for key in keys():
                self[key] = other[key]
            return
        items = getattr(other, "items", None)
        if items is not None:
            other = items()
        for pair in other:
            it = iter(pair)
            try:
                key = next(it)
            except StopIteration:
                raise ValueError("need more than 0 values to unpack") from None
            try:
                value = next(it)
            except StopIteration:
                raise ValueError("need more than 1 value to unpack") from None
            for _ in it:
                raise ValueError("too many values to unpack (expected 2)")
            self[key] = value

    def update(self, other=(), /, **kwds):
        self.__update(other, kwds)

    keys = _od_keys
    values = _od_values
    items = _od_items

    def setdefault(self, key, default=None):
        '''Insert key with a value of default if key is not in the dictionary.

        Return the value for key if key is in the dictionary, else default.
        '''
        if type(self) is OrderedDict:
            return _od_setdefault(self, key, default)
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
            state = state.copy() if state else {}
            slots = slots.copy()
            if slots:
                state = state or None, slots
            else:
                state = state or None
        return self.__class__, (), state, None, iter(self.items())

    def copy(self):
        'od.copy() -> a shallow copy of od'
        # CPython `odict_copy`: a new od of the same type, filled in order
        # (an exact od reads the dict directly, a subclass through its
        # `__getitem__`); a change to the source mid-copy raises
        # (gh-148660).
        if type(self) is OrderedDict:
            new = OrderedDict()
            for key in _od_iter(self):
                value = dict.get(self, key, _od_missing)
                if value is _od_missing:
                    raise KeyError(key)
                new[key] = value
            return new
        new = type(self)()
        for key in _od_iter(self):
            new[key] = self[key]
        return new

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
        # CPython `_odict_keys_equal`: an order-sensitive walk over both
        # orders that raises if a key comparison mutates either od
        # (gh-119004).
        return _od_keys_equal(self, other)

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
        # dict payload + one order entry per key: mirrors the C
        # implementation reporting strictly more than an equal plain dict
        # (test_sizeof).
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


_od_missing = object()
_install_odict(OrderedDict, odict_keys, odict_values, odict_items)


class _deque_reverse_iterator:
    """Reverse iterator over a live deque
    (CPython's `_collections._deque_reverse_iterator`)."""

    __slots__ = ("_deq", "_index", "_deq_state")

    def __init__(self, deq, index=0):
        if not isinstance(deq, deque):
            raise TypeError("deque expected")
        self._deq = deq
        # `index` counts consumed items, mirroring the forward iterator's
        # constructor/`__reduce__` contract.
        self._index = _deque_iterator_index(deq, index)
        self._deq_state = deq._state

    def __iter__(self):
        return self

    from _weave_collections import reverse_iterator_next as __next__

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


# The native `deque.__iter__` / `__reversed__` build these iterators
# directly (no Python `__init__` runs), so a compiled loop can capture a
# deque iterator without leaving native code. The classes ride on the
# deque class itself, so each copy of this module pairs with its own.
deque._iter_types = (_deque_iterator, _deque_reverse_iterator)
