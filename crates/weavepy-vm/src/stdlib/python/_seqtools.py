"""Internal helpers for the legacy sequence-iteration protocol.

`_SeqIter` is WeavePy's equivalent of CPython's built-in ``iterator``
type (``seqiterobject``): the object ``iter(obj)`` returns when *obj*
defines ``__getitem__`` but not ``__iter__``. It drives the sequence
**lazily** — calling ``obj[0]``, ``obj[1]``, … on demand and stopping at
the first ``IndexError`` — so an unbounded sequence (``__getitem__`` that
never raises) iterates forever instead of hanging at construction, and
side effects happen one element at a time exactly as CPython does.

It also implements the pickling protocol (``__reduce__`` /
``__setstate__``, with the CPython negative-index clamp) and
``__length_hint__`` so the standard library's iterator tests behave.
"""


async def _anext_with_default(awaitable, default):
    """Back the two-argument ``anext(aiter, default)`` builtin.

    The VM hands us the already-resolved ``__anext__`` awaitable; we
    return ``default`` when the async iterator is exhausted, matching
    CPython's ``anext`` C wrapper.
    """
    try:
        return await awaitable
    except StopAsyncIteration:
        return default


def _builtin_iter():
    """Fetch ``iter`` from the live ``builtins`` *module* namespace.

    Mirrors CPython's ``_PyEval_GetBuiltin(&_Py_ID(iter))``, which the C
    seq/callable iterator ``__reduce__`` calls *before* reading the
    iterator's state. Going through the module dict (rather than the
    bare ``iter`` global) means a user who has shadowed ``builtins.iter``
    with a hash-colliding custom key sees that key's ``__eq__`` fire here
    — the exact side-effect ordering test_iter's gh-101765 reproducer
    depends on. Falls back to the plain global if anything goes wrong.
    """
    try:
        import builtins
        return builtins.__dict__["iter"]
    except (KeyError, ImportError):
        return iter


class _SeqIter:
    __slots__ = ("_seq", "_index")

    def __init__(self, seq):
        self._seq = seq
        self._index = 0

    def __iter__(self):
        return self

    def __next__(self):
        seq = self._seq
        if seq is None:
            raise StopIteration
        try:
            item = seq[self._index]
        except (IndexError, StopIteration):
            # Exhausted: drop the sequence reference so a resurrected
            # iterator stays exhausted (matches CPython's seqiterobject,
            # which clears it_seq on both IndexError and StopIteration).
            self._seq = None
            raise StopIteration
        self._index += 1
        return item

    def __length_hint__(self):
        seq = self._seq
        if seq is None:
            return 0
        try:
            length = len(seq)
        except TypeError:
            return 0
        hint = length - self._index
        return hint if hint > 0 else 0

    def __reduce__(self):
        # Resolve `iter` first: the lookup can run user code that exhausts
        # us (gh-101765), so read `self._seq` only afterwards.
        _iter = _builtin_iter()
        if self._seq is None:
            # Exhausted iterator pickles as an empty one.
            return (_iter, ((),))
        return (_iter, (self._seq,), self._index)

    def __setstate__(self, state):
        # CPython clamps a negative resume index to 0.
        if state < 0:
            state = 0
        self._index = state


class _CallableIter:
    """WeavePy's equivalent of CPython's ``callable_iterator``
    (``calliterobject``): the object ``iter(callable, sentinel)`` returns.

    Each ``__next__`` calls *callable* with no arguments and yields the
    result, stopping (``StopIteration``) once a result compares equal to
    *sentinel*. Driving it **lazily** — one call per ``next()`` — means an
    exception raised inside *callable* propagates at the right moment (so
    ``for x in iter(spam, s)`` sees it mid-stream) and an unbounded source
    never hangs at construction, matching CPython exactly.
    """

    __slots__ = ("_callable", "_sentinel")

    def __init__(self, callable, sentinel):
        self._callable = callable
        self._sentinel = sentinel

    def __iter__(self):
        return self

    def __next__(self):
        if self._callable is None:
            raise StopIteration
        result = self._callable()
        # gh-101892: the call may have re-entered and exhausted us; if so,
        # report exhaustion rather than yielding a post-sentinel value.
        if self._callable is None:
            raise StopIteration
        # CPython compares ``result == sentinel`` (result's __eq__ first).
        if result == self._sentinel:
            self._callable = None
            raise StopIteration
        return result

    def __reduce__(self):
        # Resolve `iter` first (it can run user code that exhausts us,
        # gh-101765); an exhausted callable-iterator has dropped its
        # callable and reduces to an empty `iter(())`.
        _iter = _builtin_iter()
        if self._callable is None:
            return (_iter, ((),))
        return (_iter, (self._callable, self._sentinel))


class _FilterIter:
    """Lazy ``filter(func, iterable)`` — CPython's ``filterobject``.

    Items are pulled (and the predicate run) one ``next()`` at a time, so
    filtering an unbounded source (``filter(p, itertools.count())``)
    terminates and predicate side effects interleave with consumption
    exactly as in CPython.
    """

    __slots__ = ("_func", "_it")

    def __init__(self, func, iterable):
        self._func = func
        self._it = iter(iterable)

    def __iter__(self):
        return self

    def __next__(self):
        func = self._func
        it = self._it
        while True:
            item = next(it)
            if func is None or func is bool:
                if item:
                    return item
            elif func(item):
                return item

    def __reduce__(self):
        return (filter, (self._func, self._it))


class _MapIter:
    """Lazy ``map(func, *iterables)`` — CPython's ``mapobject``.

    ``func`` is applied on demand; iteration stops at the shortest
    iterable. Lazy evaluation means mapping over unbounded sources works
    and exceptions from ``func`` surface mid-stream, as in CPython.
    """

    __slots__ = ("_func", "_iters")

    def __init__(self, func, *iterables):
        self._func = func
        self._iters = tuple(iter(it) for it in iterables)

    def __iter__(self):
        return self

    def __next__(self):
        args = []
        for it in self._iters:
            args.append(next(it))
        return self._func(*args)

    def __reduce__(self):
        return (map, (self._func,) + self._iters)


def _zip_arg_range(count):
    """`argument 1` / `arguments 1-N` phrasing of zip-strict errors."""
    return "argument 1" if count == 1 else f"arguments 1-{count}"


class _ZipIter:
    """Lazy ``zip(*iterables, strict=...)`` — CPython's ``zipobject``.

    Stops at the shortest iterable without pre-materialising any of
    them, so zipping unbounded iterators works. With ``strict=True``,
    raises ``ValueError`` on length mismatch with CPython's wording.
    """

    __slots__ = ("_iters", "_strict")

    def __init__(self, strict, *iterables):
        self._iters = tuple(iter(it) for it in iterables)
        self._strict = strict

    def __iter__(self):
        return self

    def __next__(self):
        iters = self._iters
        if iters is None or not iters:
            raise StopIteration
        result = []
        for i, it in enumerate(iters):
            try:
                result.append(next(it))
            except StopIteration:
                if not self._strict:
                    self._iters = None
                    raise
                if i > 0:
                    self._iters = None
                    raise ValueError(
                        f"zip() argument {i+1} is shorter than {_zip_arg_range(i)}"
                    ) from None
                # First iterator exhausted: with strict the rest must be
                # exhausted too.
                for j, jt in enumerate(iters[1:], 1):
                    try:
                        next(jt)
                    except StopIteration:
                        continue
                    self._iters = None
                    raise ValueError(
                        f"zip() argument {j+1} is longer than {_zip_arg_range(j)}"
                    ) from None
                self._iters = None
                raise
        return tuple(result)

    def __reduce__(self):
        return (zip, self._iters if self._iters is not None else ((),))
