"""WeavePy's ``itertools`` module.

Every tool here is a *class* whose instances mirror CPython's C
iterator objects: the constructor signatures (positional/keyword
acceptance), ``__reduce__`` / ``__setstate__`` pickling protocol,
``__repr__`` (count / repeat), subclassing behaviour and lazy
argument-error timing all match CPython 3.13.

The data plane is split out into *cores*: when the native
``_itertools`` module is available (always, under WeavePy) each class
delegates iteration to a native core whose stepping adds no Python
frame; otherwise a pure-Python core class with an identical state
layout takes its place (which is how this module is validated against
CPython's test suite under real CPython).
"""

import sys as _sys

__all__ = [
    "accumulate",
    "batched",
    "chain",
    "combinations",
    "combinations_with_replacement",
    "compress",
    "count",
    "cycle",
    "dropwhile",
    "filterfalse",
    "groupby",
    "islice",
    "pairwise",
    "permutations",
    "product",
    "repeat",
    "starmap",
    "takewhile",
    "tee",
    "zip_longest",
]


class _NullType:
    """Internal NULL sentinel (CPython's cleared C field)."""

    def __repr__(self):
        return "<NULL>"


_NULL = _NullType()

_MAXSIZE = _sys.maxsize


def _pickle_deprecated():
    import warnings

    warnings.warn(
        "Pickle, copy, and deepcopy support will be removed from itertools in Python 3.14.",
        DeprecationWarning,
        stacklevel=3,
    )


def _no_kwargs(cls, kwargs, name):
    """Reject keyword arguments unless a subclass overrides __init__.

    CPython's clinic-generated tp_new functions skip the
    ``_PyArg_NoKeywords`` check when the instantiated type overrides
    ``tp_init`` — that's what lets ``subclass_with_init(*args, kw=1)``
    work while ``cls(*args, kw=1)`` raises.
    """
    if kwargs and cls.__init__ is object.__init__:
        raise TypeError(f"{name}() takes no keyword arguments")


def _as_int(n):
    """``operator.index`` semantics with CPython's error message."""
    if isinstance(n, int):
        return int(n)
    idx = getattr(type(n), "__index__", None)
    if idx is None:
        raise TypeError(
            f"'{type(n).__name__}' object cannot be interpreted as an integer"
        )
    return idx(n)


def _is_number(v):
    """CPython's PyNumber_Check (used by count())."""
    if isinstance(v, complex):
        return True
    t = type(v)
    return (
        hasattr(t, "__index__")
        or hasattr(t, "__int__")
        or hasattr(t, "__float__")
    )


def _is_iterator(obj):
    return hasattr(type(obj), "__next__")


# ---------------------------------------------------------------------------
# Cores. Native versions come from `_itertools`; the pure-Python twins
# below expose the exact same constructor arguments and `state()`
# layout so the wrapper classes never care which one they hold.
# ---------------------------------------------------------------------------

try:
    import _itertools as _n

    _HAVE_NATIVE = True
except ImportError:
    _HAVE_NATIVE = False


class _PyCoreBase:
    def __iter__(self):
        return self


class _PyIsliceCore(_PyCoreBase):
    __slots__ = ("source", "next_idx", "pos", "stop", "step", "done")

    def __init__(self, source, start, stop, step):
        self.source = source
        self.next_idx = start
        self.pos = 0
        self.stop = stop
        self.step = step
        self.done = False

    def __next__(self):
        # CPython's islice_next order: consume the skipped elements
        # first, *then* check the stop bound — `islice(it, 3, 3)`
        # advances `it` by three even though it yields nothing.
        if self.done:
            raise StopIteration
        src = self.source
        try:
            while self.pos < self.next_idx:
                next(src)
                self.pos += 1
            stop = self.stop
            if stop is not None and self.pos >= stop:
                self.done = True
                self.source = None
                raise StopIteration
            item = next(src)
        except StopIteration:
            self.done = True
            self.source = None
            raise
        self.pos += 1
        new_next = self.next_idx + self.step
        if self.stop is not None and new_next > self.stop:
            new_next = self.stop
        self.next_idx = new_next
        return item

    def state(self):
        return (self.source, self.next_idx, self.pos, self.stop, self.step,
                self.done)


class _PyRepeatCore(_PyCoreBase):
    __slots__ = ("obj", "times")

    def __init__(self, obj, times):
        self.obj = obj
        self.times = times

    def __next__(self):
        t = self.times
        if t is None:
            return self.obj
        if t <= 0:
            raise StopIteration
        self.times = t - 1
        return self.obj

    def state(self):
        return (self.obj, self.times)


class _PyTeeCore(_PyCoreBase):
    __slots__ = ("data", "index")

    def __init__(self, data, index):
        self.data = data
        self.index = index

    def __next__(self):
        sh = self.data
        buf = sh.buffer
        i = self.index
        if i < len(buf):
            value = buf[i]
        else:
            if sh.source is None:
                raise StopIteration
            if sh.busy:
                raise RuntimeError("cannot re-enter the tee iterator")
            sh.busy = True
            try:
                try:
                    value = next(sh.source)
                except StopIteration:
                    sh.source = None
                    raise
            finally:
                sh.busy = False
            buf.append(value)
        self.index = i + 1
        return value

    def state(self):
        return (self.data, self.index)


class _PyCountCore(_PyCoreBase):
    __slots__ = ("current", "step")

    def __init__(self, current, step):
        self.current = current
        self.step = step

    def __next__(self):
        v = self.current
        self.current = v + self.step
        return v

    def state(self):
        return (self.current, self.step)


class _PyCycleCore(_PyCoreBase):
    __slots__ = ("source", "saved", "index", "firstpass")

    def __init__(self, source, saved, index, firstpass):
        self.source = source
        self.saved = saved
        self.index = index
        self.firstpass = firstpass

    def __next__(self):
        src = self.source
        if src is not None:
            try:
                item = next(src)
            except StopIteration:
                self.source = None
            else:
                if not self.firstpass:
                    self.saved.append(item)
                return item
        saved = self.saved
        if not saved:
            raise StopIteration
        i = self.index
        item = saved[i]
        i += 1
        if i >= len(saved):
            i = 0
        self.index = i
        return item

    def state(self):
        return (self.source, self.saved, self.index, self.firstpass)


class _PyChainCore(_PyCoreBase):
    __slots__ = ("source", "active")

    def __init__(self, source, active):
        self.source = source
        self.active = active

    def __next__(self):
        while True:
            source = self.source
            if source is None:
                raise StopIteration
            active = self.active
            if active is None:
                try:
                    iterable = next(source)
                except StopIteration:
                    self.source = None
                    raise
                except BaseException:
                    self.source = None
                    raise
                try:
                    active = iter(iterable)
                except BaseException:
                    self.source = None
                    raise
                self.active = active
            try:
                return next(active)
            except StopIteration:
                self.active = None
            except BaseException:
                self.source = None
                self.active = None
                raise

    def state(self):
        return (self.source, self.active)


class _PyCompressCore(_PyCoreBase):
    __slots__ = ("data", "selectors")

    def __init__(self, data, selectors):
        self.data = data
        self.selectors = selectors

    def __next__(self):
        data = self.data
        selectors = self.selectors
        while True:
            item = next(data)
            keep = next(selectors)
            if keep:
                return item

    def state(self):
        return (self.data, self.selectors)


class _PyDropWhileCore(_PyCoreBase):
    __slots__ = ("func", "source", "started")

    def __init__(self, func, source, started):
        self.func = func
        self.source = source
        self.started = started

    def __next__(self):
        it = self.source
        func = self.func
        while True:
            item = next(it)
            if self.started:
                return item
            if not func(item):
                self.started = True
                return item

    def state(self):
        return (self.func, self.source, self.started)


class _PyTakeWhileCore(_PyCoreBase):
    __slots__ = ("func", "source", "stopped")

    def __init__(self, func, source, stopped):
        self.func = func
        self.source = source
        self.stopped = stopped

    def __next__(self):
        if self.stopped:
            raise StopIteration
        item = next(self.source)
        if self.func(item):
            return item
        self.stopped = True
        raise StopIteration

    def state(self):
        return (self.func, self.source, self.stopped)


class _PyFilterFalseCore(_PyCoreBase):
    __slots__ = ("func", "source")

    def __init__(self, func, source):
        self.func = func
        self.source = source

    def __next__(self):
        func = self.func
        it = self.source
        while True:
            item = next(it)
            if func is None or func is bool:
                if not item:
                    return item
            elif not func(item):
                return item

    def state(self):
        return (self.func, self.source)


class _PyStarMapCore(_PyCoreBase):
    __slots__ = ("func", "source")

    def __init__(self, func, source):
        self.func = func
        self.source = source

    def __next__(self):
        args = next(self.source)
        if not isinstance(args, tuple):
            args = tuple(args)
        return self.func(*args)

    def state(self):
        return (self.func, self.source)


class _PyPairwiseCore(_PyCoreBase):
    __slots__ = ("source", "old")

    def __init__(self, source):
        self.source = source
        self.old = None

    def __next__(self):
        it = self.source
        if it is None:
            raise StopIteration
        old = self.old
        if old is None:
            try:
                old = next(it)
            except StopIteration:
                self.source = None
                self.old = None
                raise
        try:
            new = next(it)
        except StopIteration:
            self.source = None
            self.old = None
            raise
        self.old = new
        return (old, new)

    def state(self):
        return (self.source, self.old)


class _PyZipLongestCore(_PyCoreBase):
    __slots__ = ("iters", "fillvalue", "numactive")

    def __init__(self, fillvalue, iters):
        self.iters = list(iters)
        self.fillvalue = fillvalue
        self.numactive = sum(1 for it in self.iters if it is not None)

    def __next__(self):
        iters = self.iters
        if not iters or self.numactive <= 0:
            raise StopIteration
        fillvalue = self.fillvalue
        result = []
        for i, it in enumerate(iters):
            if it is None:
                result.append(fillvalue)
                continue
            try:
                value = next(it)
            except StopIteration:
                self.numactive -= 1
                if self.numactive <= 0:
                    raise
                iters[i] = None
                result.append(fillvalue)
            else:
                result.append(value)
        return tuple(result)

    def state(self):
        return (self.fillvalue, self.numactive, tuple(self.iters))


class _PyAccumulateCore(_PyCoreBase):
    __slots__ = ("source", "func", "has_total", "total", "initial")

    def __init__(self, source, func, has_total, total, initial):
        self.source = source
        self.func = func
        self.has_total = bool(has_total)
        self.total = total
        self.initial = initial

    def __next__(self):
        initial = self.initial
        if initial is not None:
            self.initial = None
            self.has_total = True
            self.total = initial
            return initial
        if not self.has_total:
            total = next(self.source)
            self.has_total = True
            self.total = total
            return total
        item = next(self.source)
        func = self.func
        if func is None:
            total = self.total + item
        else:
            total = func(self.total, item)
        self.total = total
        return total

    def state(self):
        return (self.source, self.func, self.has_total, self.total,
                self.initial)


class _PyProductCore(_PyCoreBase):
    __slots__ = ("pools", "indices", "started", "stopped")

    def __init__(self, pools, indices, started, stopped):
        self.pools = pools
        self.indices = list(indices) if indices is not None else [0] * len(pools)
        self.started = started
        self.stopped = stopped

    def __next__(self):
        if self.stopped:
            raise StopIteration
        pools = self.pools
        n = len(pools)
        indices = self.indices
        if not self.started:
            for pool in pools:
                if not pool:
                    self.stopped = True
                    raise StopIteration
            self.started = True
            return tuple(pool[0] for pool in pools)
        i = n - 1
        while i >= 0:
            indices[i] += 1
            if indices[i] < len(pools[i]):
                break
            indices[i] = 0
            i -= 1
        else:
            self.stopped = True
            raise StopIteration
        return tuple(pool[idx] for pool, idx in zip(pools, indices))

    def state(self):
        return (self.pools, tuple(self.indices), self.started, self.stopped)


class _PyPermutationsCore(_PyCoreBase):
    __slots__ = ("pool", "r", "indices", "cycles", "started", "stopped")

    def __init__(self, pool, r, indices, cycles, started, stopped):
        n = len(pool)
        self.pool = pool
        self.r = r
        self.indices = list(indices) if indices is not None else list(range(n))
        self.cycles = list(cycles) if cycles is not None else list(range(n, n - r, -1))
        self.started = started
        self.stopped = stopped

    def __next__(self):
        if self.stopped:
            raise StopIteration
        pool = self.pool
        indices = self.indices
        cycles = self.cycles
        r = self.r
        n = len(pool)
        if not self.started:
            self.started = True
            return tuple(pool[indices[i]] for i in range(r))
        if not n:
            self.stopped = True
            raise StopIteration
        i = r - 1
        while i >= 0:
            cycles[i] -= 1
            if cycles[i] == 0:
                indices[i:] = indices[i + 1:] + indices[i:i + 1]
                cycles[i] = n - i
            else:
                j = cycles[i]
                indices[i], indices[-j] = indices[-j], indices[i]
                return tuple(pool[indices[k]] for k in range(r))
            i -= 1
        self.stopped = True
        raise StopIteration

    def state(self):
        return (self.pool, self.r, tuple(self.indices), tuple(self.cycles),
                self.started, self.stopped)


class _PyCombinationsCore(_PyCoreBase):
    __slots__ = ("pool", "r", "indices", "started", "stopped")

    def __init__(self, pool, r, indices, started, stopped):
        self.pool = pool
        self.r = r
        self.indices = list(indices) if indices is not None else list(range(r))
        self.started = started
        self.stopped = stopped

    def __next__(self):
        if self.stopped:
            raise StopIteration
        pool = self.pool
        indices = self.indices
        r = self.r
        n = len(pool)
        if not self.started:
            self.started = True
            return tuple(pool[i] for i in indices)
        i = r - 1
        while i >= 0 and indices[i] == i + n - r:
            i -= 1
        if i < 0:
            self.stopped = True
            raise StopIteration
        indices[i] += 1
        for j in range(i + 1, r):
            indices[j] = indices[j - 1] + 1
        return tuple(pool[i] for i in indices)

    def state(self):
        return (self.pool, self.r, tuple(self.indices), self.started,
                self.stopped)


class _PyCwrCore(_PyCoreBase):
    __slots__ = ("pool", "r", "indices", "started", "stopped")

    def __init__(self, pool, r, indices, started, stopped):
        self.pool = pool
        self.r = r
        self.indices = list(indices) if indices is not None else [0] * r
        self.started = started
        self.stopped = stopped

    def __next__(self):
        if self.stopped:
            raise StopIteration
        pool = self.pool
        indices = self.indices
        r = self.r
        n = len(pool)
        if not self.started:
            self.started = True
            return tuple(pool[i] for i in indices)
        i = r - 1
        while i >= 0 and indices[i] == n - 1:
            i -= 1
        if i < 0:
            self.stopped = True
            raise StopIteration
        indices[i:] = [indices[i] + 1] * (r - i)
        return tuple(pool[i] for i in indices)

    def state(self):
        return (self.pool, self.r, tuple(self.indices), self.started,
                self.stopped)


class _PyBatchedCore(_PyCoreBase):
    __slots__ = ("source", "n", "strict")

    def __init__(self, source, n, strict):
        self.source = source
        self.n = n
        self.strict = strict

    def __next__(self):
        it = self.source
        if it is None:
            raise StopIteration
        batch = []
        try:
            for _ in range(self.n):
                batch.append(next(it))
        except StopIteration:
            self.source = None
            if not batch:
                raise
            if self.strict:
                raise ValueError("batched(): incomplete batch") from None
        return tuple(batch)

    def state(self):
        return (self.source, self.n, self.strict)


if _HAVE_NATIVE:
    _islice_core = _n.islice_core
    _repeat_core = _n.repeat_core
    _tee_core = _n.tee_core
    _count_core = _n.count_core
    _cycle_core = _n.cycle_core
    _chain_core = _n.chain_core
    _compress_core = _n.compress_core
    _dropwhile_core = _n.dropwhile_core
    _takewhile_core = _n.takewhile_core
    _filterfalse_core = _n.filterfalse_core
    _starmap_core = _n.starmap_core
    _pairwise_core = _n.pairwise_core
    _zip_longest_core = _n.zip_longest_core
    _accumulate_core = _n.accumulate_core
    _product_core = _n.product_core
    _permutations_core = _n.permutations_core
    _combinations_core = _n.combinations_core
    _cwr_core = _n.cwr_core
    _batched_core = _n.batched_core
    _core_state = _n.lazy_state

    def _islice_set_cnt(core, cnt):
        _n.islice_set_cnt(core, cnt)
else:
    _islice_core = _PyIsliceCore
    _repeat_core = _PyRepeatCore
    _tee_core = _PyTeeCore
    _count_core = _PyCountCore
    _cycle_core = _PyCycleCore
    _chain_core = _PyChainCore
    _compress_core = _PyCompressCore
    _dropwhile_core = _PyDropWhileCore
    _takewhile_core = _PyTakeWhileCore
    _filterfalse_core = _PyFilterFalseCore
    _starmap_core = _PyStarMapCore
    _pairwise_core = _PyPairwiseCore
    _zip_longest_core = _PyZipLongestCore
    _accumulate_core = _PyAccumulateCore
    _product_core = _PyProductCore
    _permutations_core = _PyPermutationsCore
    _combinations_core = _PyCombinationsCore
    _cwr_core = _PyCwrCore
    _batched_core = _PyBatchedCore

    def _core_state(core):
        return core.state()

    def _islice_set_cnt(core, cnt):
        core.pos = cnt


# ---------------------------------------------------------------------------
# count
# ---------------------------------------------------------------------------

class count:
    """count(start=0, step=1) --> count object

    Return a count object whose .__next__() method returns consecutive
    values.
    """

    def __new__(cls, start=0, step=1):
        if not _is_number(start) or not _is_number(step):
            raise TypeError("a number is required")
        self = object.__new__(cls)
        self._core = _count_core(start, step)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __repr__(self):
        cnt, step = _core_state(self._core)
        if type(step) is int and step == 1:
            return f"count({cnt!r})"
        return f"count({cnt!r}, {step!r})"

    def __reduce__(self):
        _pickle_deprecated()
        cnt, step = _core_state(self._core)
        if type(step) is int and step == 1:
            return (type(self), (cnt,))
        return (type(self), (cnt, step))


# ---------------------------------------------------------------------------
# cycle
# ---------------------------------------------------------------------------

class cycle:
    """cycle(iterable) --> cycle object

    Return elements from the iterable until it is exhausted.  Then
    repeat the sequence indefinitely.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "cycle")
        if len(args) != 1:
            raise TypeError(
                f"cycle expected 1 argument, got {len(args)}"
            )
        self = object.__new__(cls)
        self._core = _cycle_core(iter(args[0]), [], 0, False)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        source, saved, index, firstpass = _core_state(self._core)
        if source is None:
            it = iter(saved)
            if index:
                it.__setstate__(index)
            return (type(self), (it,), (saved, True))
        return (type(self), (source,), (saved, bool(firstpass)))

    def __setstate__(self, state):
        _pickle_deprecated()
        if not isinstance(state, tuple):
            raise TypeError("state is not a tuple")
        if len(state) != 2:
            raise TypeError("state is not a 2-tuple")
        saved, firstpass = state
        if not isinstance(saved, list):
            raise TypeError("saved is not a list")
        if isinstance(firstpass, bool):
            firstpass = int(firstpass)
        elif not isinstance(firstpass, int):
            raise TypeError(
                f"'{type(firstpass).__name__}' object cannot be interpreted as an integer"
            )
        source, _, _, _ = _core_state(self._core)
        self._core = _cycle_core(source, saved, 0, bool(firstpass))


# ---------------------------------------------------------------------------
# repeat
# ---------------------------------------------------------------------------

class repeat:
    """repeat(object [,times]) -> create an iterator which returns the
    object for the specified number of times.  If not specified, returns
    the object endlessly.
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        if kwargs:
            allowed = {"object", "times"}
            for k in kwargs:
                if k not in allowed:
                    raise TypeError(
                        f"repeat() got an unexpected keyword argument '{k}'"
                    )
            if nargs >= 1 and "object" in kwargs:
                raise TypeError("argument for repeat() given by name ('object') and position (1)")
            if nargs >= 2 and "times" in kwargs:
                raise TypeError("argument for repeat() given by name ('times') and position (2)")
        if nargs > 2:
            raise TypeError(f"repeat expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            obj = args[0]
        elif "object" in kwargs:
            obj = kwargs["object"]
        else:
            raise TypeError("repeat() missing required argument 'object' (pos 1)")
        if nargs == 2:
            times = args[1]
        elif "times" in kwargs:
            times = kwargs["times"]
        else:
            times = None
        if times is not None:
            times = _as_int(times)
            if times < 0:
                times = 0
        self = object.__new__(cls)
        self._core = _repeat_core(obj, times)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __length_hint__(self):
        obj, times = _core_state(self._core)
        if times is None:
            raise TypeError("len() of unsized object")
        return times

    def __repr__(self):
        obj, times = _core_state(self._core)
        if times is None:
            return f"repeat({obj!r})"
        return f"repeat({obj!r}, {times})"

    def __reduce__(self):
        _pickle_deprecated()
        obj, times = _core_state(self._core)
        if times is None:
            return (type(self), (obj,))
        return (type(self), (obj, times))


# ---------------------------------------------------------------------------
# accumulate
# ---------------------------------------------------------------------------

class accumulate:
    """accumulate(iterable[, func, *, initial=None]) --> accumulate object

    Return series of accumulated sums (or other binary function
    results).
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        iterable = _NULL
        func = None
        initial = None
        if kwargs:
            for k in kwargs:
                if k not in ("iterable", "func", "initial"):
                    raise TypeError(
                        f"accumulate() got an unexpected keyword argument '{k}'"
                    )
            if "iterable" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for accumulate() given by name ('iterable') and position (1)"
                    )
                iterable = kwargs["iterable"]
            if "func" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for accumulate() given by name ('func') and position (2)"
                    )
                func = kwargs["func"]
            initial = kwargs.get("initial")
        if nargs > 2:
            raise TypeError(f"accumulate expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            iterable = args[0]
        if nargs == 2:
            func = args[1]
        if iterable is _NULL:
            raise TypeError("accumulate() missing required argument 'iterable' (pos 1)")
        self = object.__new__(cls)
        self._core = _accumulate_core(iter(iterable), func, False, None, initial)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        source, func, has_total, total, initial = _core_state(self._core)
        return (
            type(self),
            (source, func),
            (bool(has_total), total if has_total else None, initial),
        )

    def __setstate__(self, state):
        _pickle_deprecated()
        source, func, _, _, _ = _core_state(self._core)
        if isinstance(state, tuple) and len(state) == 3:
            has_total, total, initial = state
            self._core = _accumulate_core(
                source, func, bool(has_total), total, initial
            )
        else:
            # CPython-style state: the running total.
            self._core = _accumulate_core(source, func, True, state, None)


# ---------------------------------------------------------------------------
# chain
# ---------------------------------------------------------------------------

class chain:
    """chain(*iterables) --> chain object

    Return a chain object whose .__next__() method returns elements
    from the first iterable until it is exhausted, then elements from
    the next iterable, until all of the iterables are exhausted.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "chain")
        self = object.__new__(cls)
        self._core = _chain_core(iter(args), None)
        return self

    @classmethod
    def from_iterable(cls, iterable):
        self = object.__new__(cls)
        self._core = _chain_core(iter(iterable), None)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        source, active = _core_state(self._core)
        if source is None:
            return (type(self), ())
        if active is None:
            return (type(self), (), (source,))
        return (type(self), (), (source, active))

    def __setstate__(self, state):
        _pickle_deprecated()
        if not isinstance(state, tuple):
            raise TypeError("state is not a tuple")
        if not 1 <= len(state) <= 2:
            raise TypeError("state is not a 1- or 2-tuple")
        if not _is_iterator(state[0]):
            raise TypeError("Arguments must be iterators.")
        if len(state) == 2 and not _is_iterator(state[1]):
            raise TypeError("Arguments must be iterators.")
        self._core = _chain_core(
            state[0], state[1] if len(state) == 2 else None
        )


# ---------------------------------------------------------------------------
# compress
# ---------------------------------------------------------------------------

class compress:
    """compress(data, selectors) --> iterator over selected data

    Return data elements corresponding to true selector elements.
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        data = _NULL
        selectors = _NULL
        if kwargs:
            for k in kwargs:
                if k not in ("data", "selectors"):
                    raise TypeError(
                        f"compress() got an unexpected keyword argument '{k}'"
                    )
            if "data" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for compress() given by name ('data') and position (1)"
                    )
                data = kwargs["data"]
            if "selectors" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for compress() given by name ('selectors') and position (2)"
                    )
                selectors = kwargs["selectors"]
        if nargs > 2:
            raise TypeError(f"compress expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            data = args[0]
        if nargs == 2:
            selectors = args[1]
        if data is _NULL:
            raise TypeError("compress() missing required argument 'data' (pos 1)")
        if selectors is _NULL:
            raise TypeError("compress() missing required argument 'selectors' (pos 2)")
        self = object.__new__(cls)
        self._core = _compress_core(iter(data), iter(selectors))
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        data, selectors = _core_state(self._core)
        return (type(self), (data, selectors))


# ---------------------------------------------------------------------------
# dropwhile / takewhile / filterfalse / starmap
# ---------------------------------------------------------------------------

class dropwhile:
    """dropwhile(predicate, iterable) --> dropwhile object

    Drop items from the iterable while predicate(item) is true.
    Afterwards, return every element until the iterable is exhausted.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "dropwhile")
        if len(args) != 2:
            raise TypeError(f"dropwhile expected 2 arguments, got {len(args)}")
        self = object.__new__(cls)
        self._core = _dropwhile_core(args[0], iter(args[1]), False)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        func, source, started = _core_state(self._core)
        return (type(self), (func, source), int(started))

    def __setstate__(self, state):
        _pickle_deprecated()
        func, source, _ = _core_state(self._core)
        self._core = _dropwhile_core(func, source, bool(state))


class takewhile:
    """takewhile(predicate, iterable) --> takewhile object

    Return successive entries from an iterable as long as the
    predicate evaluates to true for each entry.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "takewhile")
        if len(args) != 2:
            raise TypeError(f"takewhile expected 2 arguments, got {len(args)}")
        self = object.__new__(cls)
        self._core = _takewhile_core(args[0], iter(args[1]), False)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        func, source, stopped = _core_state(self._core)
        return (type(self), (func, source), int(stopped))

    def __setstate__(self, state):
        _pickle_deprecated()
        func, source, _ = _core_state(self._core)
        self._core = _takewhile_core(func, source, bool(state))


class filterfalse:
    """filterfalse(function or None, sequence) --> filterfalse object

    Return those items of sequence for which function(item) is false.
    If function is None, return the items that are false.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "filterfalse")
        if len(args) != 2:
            raise TypeError(f"filterfalse expected 2 arguments, got {len(args)}")
        self = object.__new__(cls)
        self._core = _filterfalse_core(args[0], iter(args[1]))
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        func, source = _core_state(self._core)
        return (type(self), (func, source))


class starmap:
    """starmap(function, sequence) --> starmap object

    Return an iterator whose values are returned from the function
    evaluated with an argument tuple taken from the given sequence.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "starmap")
        if len(args) != 2:
            raise TypeError(f"starmap expected 2 arguments, got {len(args)}")
        self = object.__new__(cls)
        self._core = _starmap_core(args[0], iter(args[1]))
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        func, source = _core_state(self._core)
        return (type(self), (func, source))


# ---------------------------------------------------------------------------
# groupby / _grouper
# ---------------------------------------------------------------------------

class groupby:
    """groupby(iterable, key=None) -> make an iterator that returns
    consecutive keys and groups from the iterable.
    """

    def __new__(cls, iterable=_NULL, key=None, *rest):
        if rest:
            raise TypeError(
                f"groupby() takes at most 2 arguments ({2 + len(rest)} given)"
            )
        if iterable is _NULL:
            raise TypeError("groupby() missing required argument 'iterable' (pos 1)")
        self = object.__new__(cls)
        self._it = iter(iterable)
        self._keyfunc = key
        self._tgtkey = _NULL
        self._currkey = _NULL
        self._currvalue = _NULL
        self._currgrouper = None
        return self

    def __iter__(self):
        return self

    def _groupby_step(self):
        # Pull the next (value, key) pair; assign both only after both
        # succeeded (CPython groupby_step's temporaries — re-entrant
        # key functions must not observe a half-updated state).
        newvalue = next(self._it)
        keyfunc = self._keyfunc
        if keyfunc is None:
            newkey = newvalue
        else:
            newkey = keyfunc(newvalue)
        self._currkey = newkey
        self._currvalue = newvalue

    def __next__(self):
        self._currgrouper = None
        # Skip to the next iteration group.
        while True:
            currkey = self._currkey
            if currkey is _NULL:
                pass
            elif self._tgtkey is _NULL:
                break
            else:
                tgtkey = self._tgtkey
                if not (tgtkey is currkey or tgtkey == currkey):
                    break
            self._groupby_step()
        self._tgtkey = self._currkey
        grouper = _grouper(self, self._tgtkey)
        self._currgrouper = grouper
        return (self._currkey, grouper)

    def __reduce__(self):
        _pickle_deprecated()
        if (
            self._tgtkey is not _NULL
            and self._currkey is not _NULL
            and self._currvalue is not _NULL
        ):
            return (
                type(self),
                (self._it, self._keyfunc),
                (self._currkey, self._currvalue, self._tgtkey),
            )
        return (type(self), (self._it, self._keyfunc))

    def __setstate__(self, state):
        _pickle_deprecated()
        if not (isinstance(state, tuple) and len(state) == 3):
            raise TypeError("state is not a 3-tuple")
        currkey, currvalue, tgtkey = state
        self._currkey = currkey
        self._currvalue = currvalue
        self._tgtkey = tgtkey


class _grouper:

    def __new__(cls, parent, tgtkey):
        if not isinstance(parent, groupby):
            raise TypeError("incorrect usage of internal _grouper")
        self = object.__new__(cls)
        self._parent = parent
        self._tgtkey = tgtkey
        # CPython's _grouper_new runs _grouper_create, which installs
        # the new grouper as the parent's current one — unpickled
        # groupers must resume the in-progress group.
        parent._currgrouper = self
        return self

    def __iter__(self):
        return self

    def __next__(self):
        gbo = self._parent
        if gbo._currgrouper is not self:
            raise StopIteration
        if gbo._currvalue is _NULL:
            gbo._groupby_step()
        currkey = gbo._currkey
        tgtkey = self._tgtkey
        if not (tgtkey is currkey or tgtkey == currkey):
            raise StopIteration
        # The == above may run user __eq__ that re-enters this grouper
        # (gh-146613) — re-check the state it could have mutated.
        if gbo._currgrouper is not self or gbo._currvalue is _NULL:
            raise StopIteration
        r = gbo._currvalue
        gbo._currvalue = _NULL
        gbo._currkey = _NULL
        return r

    def __reduce__(self):
        _pickle_deprecated()
        if self._parent._currgrouper is self:
            return (type(self), (self._parent, self._tgtkey))
        return (iter, ((),))


# ---------------------------------------------------------------------------
# islice
# ---------------------------------------------------------------------------

def _islice_index(arg, what):
    if arg is None:
        return None
    try:
        value = _as_int(arg)
    except TypeError:
        raise ValueError(
            f"{what} for islice() must be None or an integer: 0 <= x <= sys.maxsize."
        ) from None
    if not 0 <= value <= _MAXSIZE:
        raise ValueError(
            f"{what} for islice() must be None or an integer: 0 <= x <= sys.maxsize."
        )
    return value


class islice:
    """islice(iterable, stop) --> islice object
    islice(iterable, start, stop[, step]) --> islice object

    Return an iterator whose next() method returns selected values
    from an iterable.
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "islice")
        nargs = len(args)
        if nargs < 2:
            raise TypeError(f"islice expected at least 2 arguments, got {nargs}")
        if nargs > 4:
            raise TypeError(f"islice expected at most 4 arguments, got {nargs}")
        iterable = args[0]
        if nargs == 2:
            stop = _islice_index(args[1], "Stop argument")
            start, step = 0, 1
        else:
            start = _islice_index(args[1], "Indices")
            if start is None:
                start = 0
            stop = _islice_index(args[2], "Stop argument")
            if nargs == 4:
                step_arg = args[3]
                if step_arg is None:
                    step = 1
                else:
                    try:
                        step = _as_int(step_arg)
                    except TypeError:
                        raise ValueError(
                            "Step for islice() must be a positive integer or None."
                        ) from None
                    if step < 1:
                        raise ValueError(
                            "Step for islice() must be a positive integer or None."
                        )
            else:
                step = 1
        self = object.__new__(cls)
        self._core = _islice_core(iter(iterable), start, stop, step)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        source, next_idx, pos, stop, step, done = _core_state(self._core)
        if source is None or done:
            return (type(self), (iter(()), 0), 0)
        return (type(self), (source, next_idx, stop, step), pos)

    def __setstate__(self, state):
        _pickle_deprecated()
        _islice_set_cnt(self._core, _as_int(state))


# ---------------------------------------------------------------------------
# pairwise
# ---------------------------------------------------------------------------

class pairwise:
    """pairwise(iterable) --> iterator of consecutive overlapping pairs

    s -> (s[0],s[1]), (s[1],s[2]), (s[2], s[3]), ...
    """

    def __new__(cls, *args, **kwargs):
        _no_kwargs(cls, kwargs, "pairwise")
        if len(args) != 1:
            raise TypeError(f"pairwise expected 1 argument, got {len(args)}")
        self = object.__new__(cls)
        self._core = _pairwise_core(iter(args[0]))
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)


# ---------------------------------------------------------------------------
# zip_longest
# ---------------------------------------------------------------------------

class zip_longest:
    """zip_longest(iter1 [,iter2 [...]], [fillvalue=None]) --> zip_longest object

    Return a zip_longest object whose .__next__() method returns a
    tuple where the i-th element comes from the i-th iterable argument.
    """

    def __new__(cls, *args, **kwargs):
        fillvalue = None
        if kwargs:
            if "fillvalue" in kwargs:
                fillvalue = kwargs.pop("fillvalue")
            if kwargs and cls.__init__ is object.__init__:
                k = next(iter(kwargs))
                raise TypeError(
                    f"zip_longest() got an unexpected keyword argument '{k}'"
                )
        self = object.__new__(cls)
        self._core = _zip_longest_core(
            fillvalue, tuple(iter(it) for it in args)
        )
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        fillvalue, numactive, slots = _core_state(self._core)
        iters = tuple(iter(()) if it is None else it for it in slots)
        return (type(self), iters, fillvalue)

    def __setstate__(self, state):
        _pickle_deprecated()
        _, _, slots = _core_state(self._core)
        self._core = _zip_longest_core(state, slots)


# ---------------------------------------------------------------------------
# product
# ---------------------------------------------------------------------------

class product:
    """product(*iterables, repeat=1) --> product object

    Cartesian product of input iterables.  Equivalent to nested
    for-loops.
    """

    def __new__(cls, *args, **kwargs):
        nrepeat = 1
        if kwargs:
            if "repeat" in kwargs:
                nrepeat = _as_int(kwargs.pop("repeat"))
            if kwargs and cls.__init__ is object.__init__:
                k = next(iter(kwargs))
                raise TypeError(
                    f"product() got an unexpected keyword argument '{k}'"
                )
        if nrepeat < 0:
            raise ValueError("repeat argument cannot be negative")
        self = object.__new__(cls)
        pools = tuple([tuple(it) for it in args] * nrepeat)
        self._core = _product_core(pools, None, False, False)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        pools, indices, started, stopped = _core_state(self._core)
        if stopped:
            return (type(self), ((),))
        if not started:
            return (type(self), tuple(pools))
        return (type(self), tuple(pools), tuple(indices))

    def __setstate__(self, state):
        _pickle_deprecated()
        pools, _, _, stopped = _core_state(self._core)
        if not isinstance(state, tuple) or len(state) != len(pools):
            raise TypeError("invalid arguments")
        indices = []
        for index, pool in zip(state, pools):
            index = _as_int(index)
            poolsize = len(pool)
            if poolsize == 0:
                self._core = _product_core(tuple(pools), None, True, True)
                return
            if index < 0:
                index = 0
            elif index > poolsize - 1:
                index = poolsize - 1
            indices.append(index)
        # Mark as started: the next __next__ advances past `indices`.
        self._core = _product_core(
            tuple(pools), tuple(indices), True, bool(stopped)
        )


# ---------------------------------------------------------------------------
# permutations / combinations / combinations_with_replacement
# ---------------------------------------------------------------------------

class permutations:
    """permutations(iterable[, r]) --> permutations object

    Return successive r-length permutations of elements in the
    iterable.
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        iterable = _NULL
        r = None
        if kwargs:
            for k in kwargs:
                if k not in ("iterable", "r"):
                    raise TypeError(
                        f"permutations() got an unexpected keyword argument '{k}'"
                    )
            if "iterable" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for permutations() given by name ('iterable') and position (1)"
                    )
                iterable = kwargs["iterable"]
            if "r" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for permutations() given by name ('r') and position (2)"
                    )
                r = kwargs["r"]
        if nargs > 2:
            raise TypeError(f"permutations expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            iterable = args[0]
        if nargs == 2:
            r = args[1]
        if iterable is _NULL:
            raise TypeError("permutations() missing required argument 'iterable' (pos 1)")
        pool = tuple(iterable)
        n = len(pool)
        if r is None:
            r = n
        else:
            if not isinstance(r, int):
                try:
                    r = _as_int(r)
                except TypeError:
                    raise TypeError("Expected int as r") from None
            if r < 0:
                raise ValueError("r must be non-negative")
        self = object.__new__(cls)
        self._core = _permutations_core(pool, r, None, None, False, r > n)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        pool, r, indices, cycles, started, stopped = _core_state(self._core)
        if stopped:
            return (type(self), ((), r))
        if not started:
            return (type(self), (pool, r))
        return (type(self), (pool, r), (tuple(indices), tuple(cycles)))

    def __setstate__(self, state):
        _pickle_deprecated()
        if not (isinstance(state, tuple) and len(state) == 2):
            raise TypeError("invalid arguments")
        indices, cycles = state
        pool, r, _, _, _, stopped = _core_state(self._core)
        n = len(pool)
        if len(indices) != n or len(cycles) != r:
            raise ValueError("invalid arguments")
        indices = [min(max(_as_int(i), 0), n - 1) for i in indices]
        cycles = [min(max(_as_int(c), 1), n - i) for i, c in enumerate(cycles)]
        self._core = _permutations_core(
            pool, r, tuple(indices), tuple(cycles), True, bool(stopped)
        )


class combinations:
    """combinations(iterable, r) --> combinations object

    Return successive r-length combinations of elements in the
    iterable.
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        iterable = _NULL
        r = _NULL
        if kwargs:
            for k in kwargs:
                if k not in ("iterable", "r"):
                    raise TypeError(
                        f"combinations() got an unexpected keyword argument '{k}'"
                    )
            if "iterable" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for combinations() given by name ('iterable') and position (1)"
                    )
                iterable = kwargs["iterable"]
            if "r" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for combinations() given by name ('r') and position (2)"
                    )
                r = kwargs["r"]
        if nargs > 2:
            raise TypeError(f"combinations expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            iterable = args[0]
        if nargs == 2:
            r = args[1]
        if iterable is _NULL:
            raise TypeError("combinations() missing required argument 'iterable' (pos 1)")
        if r is _NULL:
            raise TypeError("combinations() missing required argument 'r' (pos 2)")
        pool = tuple(iterable)
        r = _as_int(r)
        if r < 0:
            raise ValueError("r must be non-negative")
        self = object.__new__(cls)
        self._core = _combinations_core(pool, r, None, False, r > len(pool))
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        pool, r, indices, started, stopped = _core_state(self._core)
        if stopped:
            return (type(self), ((), r))
        if not started:
            return (type(self), (pool, r))
        return (type(self), (pool, r), tuple(indices))

    def __setstate__(self, state):
        _pickle_deprecated()
        pool, r, _, _, stopped = _core_state(self._core)
        if not isinstance(state, tuple) or len(state) != r:
            raise TypeError("invalid arguments")
        n = len(pool)
        indices = []
        for i, index in enumerate(state):
            index = _as_int(index)
            maxval = i + n - r
            if index < 0:
                index = 0
            elif index > maxval:
                index = maxval
            indices.append(index)
        self._core = _combinations_core(
            pool, r, tuple(indices), True, bool(stopped)
        )


class combinations_with_replacement:
    """combinations_with_replacement(iterable, r) --> combinations_with_replacement object

    Return successive r-length combinations of elements in the
    iterable allowing individual elements to have successive repeats.
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        iterable = _NULL
        r = _NULL
        if kwargs:
            for k in kwargs:
                if k not in ("iterable", "r"):
                    raise TypeError(
                        "combinations_with_replacement() got an unexpected "
                        f"keyword argument '{k}'"
                    )
            if "iterable" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for combinations_with_replacement() given by "
                        "name ('iterable') and position (1)"
                    )
                iterable = kwargs["iterable"]
            if "r" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for combinations_with_replacement() given by "
                        "name ('r') and position (2)"
                    )
                r = kwargs["r"]
        if nargs > 2:
            raise TypeError(
                f"combinations_with_replacement expected at most 2 arguments, got {nargs}"
            )
        if nargs >= 1:
            iterable = args[0]
        if nargs == 2:
            r = args[1]
        if iterable is _NULL:
            raise TypeError(
                "combinations_with_replacement() missing required argument 'iterable' (pos 1)"
            )
        if r is _NULL:
            raise TypeError(
                "combinations_with_replacement() missing required argument 'r' (pos 2)"
            )
        pool = tuple(iterable)
        r = _as_int(r)
        if r < 0:
            raise ValueError("r must be non-negative")
        self = object.__new__(cls)
        self._core = _cwr_core(pool, r, None, False, not pool and r > 0)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __reduce__(self):
        _pickle_deprecated()
        pool, r, indices, started, stopped = _core_state(self._core)
        if stopped:
            return (type(self), ((), r))
        if not started:
            return (type(self), (pool, r))
        return (type(self), (pool, r), tuple(indices))

    def __setstate__(self, state):
        _pickle_deprecated()
        pool, r, _, _, stopped = _core_state(self._core)
        if not isinstance(state, tuple) or len(state) != r:
            raise TypeError("invalid arguments")
        n = len(pool)
        indices = []
        for index in state:
            index = _as_int(index)
            if index < 0:
                index = 0
            elif index > n - 1:
                index = n - 1
            indices.append(index)
        self._core = _cwr_core(pool, r, tuple(indices), True, bool(stopped))


# ---------------------------------------------------------------------------
# batched
# ---------------------------------------------------------------------------

class batched:
    """batched(iterable, n, *, strict=False) --> batched object

    Batch data into tuples of length n. The last batch may be shorter
    than n (unless strict is true, in which case it raises ValueError).
    """

    def __new__(cls, *args, **kwargs):
        nargs = len(args)
        iterable = _NULL
        n = _NULL
        strict = False
        if kwargs:
            for k in kwargs:
                if k not in ("iterable", "n", "strict"):
                    raise TypeError(
                        f"batched() got an unexpected keyword argument '{k}'"
                    )
            if "iterable" in kwargs:
                if nargs >= 1:
                    raise TypeError(
                        "argument for batched() given by name ('iterable') and position (1)"
                    )
                iterable = kwargs["iterable"]
            if "n" in kwargs:
                if nargs >= 2:
                    raise TypeError(
                        "argument for batched() given by name ('n') and position (2)"
                    )
                n = kwargs["n"]
            strict = bool(kwargs.get("strict", False))
        if nargs > 2:
            raise TypeError(f"batched expected at most 2 arguments, got {nargs}")
        if nargs >= 1:
            iterable = args[0]
        if nargs == 2:
            n = args[1]
        if iterable is _NULL:
            raise TypeError("batched() missing required argument 'iterable' (pos 1)")
        if n is _NULL:
            raise TypeError("batched() missing required argument 'n' (pos 2)")
        n = _as_int(n)
        if n < 1:
            raise ValueError("n must be at least one")
        self = object.__new__(cls)
        self._core = _batched_core(iter(iterable), n, strict)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)


# ---------------------------------------------------------------------------
# tee
# ---------------------------------------------------------------------------

class _tee_dataobject:
    """Data container shared by the branches of one ``tee()`` call.

    Pickling two branches of the same tee in one dump must reconnect
    them to a single shared buffer on load — sharing happens through
    this object (pickle's memo deduplicates it), like CPython's
    ``itertools._tee_dataobject``.
    """

    def __init__(self, source):
        self.source = source
        self.buffer = []
        self.busy = False

    def __reduce__(self):
        _pickle_deprecated()
        return (_tee_dataobject_reconstruct, (self.source, self.buffer))


def _tee_dataobject_reconstruct(source, buffer):
    data = _tee_dataobject(source)
    data.buffer = buffer
    return data


class _tee:
    """Iterator wrapped to make it copyable."""

    def __new__(cls, iterable):
        if isinstance(iterable, _tee):
            return iterable.__copy__()
        self = object.__new__(cls)
        self._data = _tee_dataobject(iter(iterable))
        self._core = _tee_core(self._data, 0)
        return self

    @classmethod
    def _from_data(cls, data, index):
        self = object.__new__(cls)
        self._data = data
        self._core = _tee_core(data, index)
        return self

    def __iter__(self):
        return self._core

    def __next__(self):
        return next(self._core)

    def __copy__(self):
        data, index = _core_state(self._core)
        return type(self)._from_data(data, index)

    def __reduce__(self):
        _pickle_deprecated()
        data, index = _core_state(self._core)
        return (type(self), ((),), (data, index))

    def __setstate__(self, state):
        _pickle_deprecated()
        if not (isinstance(state, tuple) and len(state) == 2):
            raise TypeError("state is not a 2-tuple")
        data, index = state
        if not isinstance(data, _tee_dataobject):
            raise TypeError("state is not a _tee_dataobject")
        self._data = data
        self._core = _tee_core(data, _as_int(index))


def tee(iterable, n=2):
    """tee(iterable, n=2) --> tuple of n independent iterators."""
    n = _as_int(n)
    if n < 0:
        raise ValueError("n must be >= 0")
    if n == 0:
        return ()
    first = _tee(iterable)
    result = [first]
    for _ in range(n - 1):
        result.append(_tee(first))
    return tuple(result)
