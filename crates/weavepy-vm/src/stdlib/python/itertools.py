"""WeavePy's pure-Python ``itertools`` module.

Implementations are generators that yield lazily, matching the
behaviour of CPython's C implementations closely enough for everyday
use. Type-checking is intentionally permissive: anything iterable
works.
"""

__all__ = [
    "count",
    "cycle",
    "repeat",
    "accumulate",
    "chain",
    "compress",
    "dropwhile",
    "filterfalse",
    "groupby",
    "islice",
    "starmap",
    "takewhile",
    "tee",
    "zip_longest",
    "product",
    "permutations",
    "combinations",
    "combinations_with_replacement",
    "pairwise",
    "batched",
]


def count(start=0, step=1):
    n = start
    while True:
        yield n
        n += step


def cycle(iterable):
    saved = []
    for item in iterable:
        yield item
        saved.append(item)
    while saved:
        for item in saved:
            yield item


def repeat(obj, times=None):
    if times is None:
        while True:
            yield obj
    else:
        i = 0
        while i < times:
            yield obj
            i += 1


def accumulate(iterable, func=None, *, initial=None):
    it = iter(iterable)
    if initial is not None:
        total = initial
        yield total
    else:
        try:
            total = next(it)
        except StopIteration:
            return
        yield total
    for item in it:
        if func is None:
            total = total + item
        else:
            total = func(total, item)
        yield total


def chain(*iterables):
    for iterable in iterables:
        for item in iterable:
            yield item


def chain_from_iterable(iterables):
    """Equivalent to :func:`chain.from_iterable`. Kept as a module
    level binding for callers that look it up by name."""
    for inner in iterables:
        for item in inner:
            yield item


chain.from_iterable = chain_from_iterable


def compress(data, selectors):
    for item, keep in zip(data, selectors):
        if keep:
            yield item


def dropwhile(predicate, iterable):
    it = iter(iterable)
    for item in it:
        if not predicate(item):
            yield item
            break
    for item in it:
        yield item


def filterfalse(predicate, iterable):
    if predicate is None:
        predicate = bool
    for item in iterable:
        if not predicate(item):
            yield item


def groupby(iterable, key=None):
    if key is None:
        key = lambda x: x
    it = iter(iterable)
    try:
        current = next(it)
    except StopIteration:
        return
    current_key = key(current)
    group = [current]
    for item in it:
        k = key(item)
        if k == current_key:
            group.append(item)
        else:
            yield current_key, iter(group)
            current_key = k
            group = [item]
    yield current_key, iter(group)


def islice(iterable, *args):
    if len(args) == 1:
        start, stop, step = 0, args[0], 1
    elif len(args) == 2:
        start, stop = args
        step = 1
    elif len(args) == 3:
        start, stop, step = args
    else:
        raise TypeError("islice expects 1-3 args")
    if step is None:
        step = 1
    if step < 1:
        raise ValueError("step must be >= 1")
    it = iter(iterable)
    i = 0
    next_idx = start
    for item in it:
        if stop is not None and i >= stop:
            return
        if i == next_idx:
            yield item
            next_idx += step
        i += 1


# Prefer the native islice: like CPython's C implementation, stepping
# it adds no Python frame — `traceback.walk_stack`'s hardcoded `f_back`
# hop count through `StackSummary.extract` depends on that.
try:
    from _itertools import islice
except ImportError:
    pass


def starmap(func, iterable):
    for args in iterable:
        yield func(*args)


def takewhile(predicate, iterable):
    for item in iterable:
        if not predicate(item):
            return
        yield item


class _TeeState:
    """Source iterator shared by the branches of one ``tee()`` call.

    ``busy`` guards the source pull: CPython's C ``tee`` raises
    ``RuntimeError`` when one branch tries to advance the shared source
    while another is already blocked inside it (test_tee_concurrent).
    """

    __slots__ = ("it", "busy")

    def __init__(self, it):
        self.it = it
        self.busy = False


class _TeeIter:
    """One branch of a lazy ``tee()``.

    Branches share a singly-linked buffer of ``[value, next_link]``
    cells; ``next_link is None`` marks the frontier where the source
    iterator must be advanced. The source is consumed on demand, so
    ``tee`` works on infinite and partially-consumed iterators.
    """

    __slots__ = ("_state", "_link")

    def __init__(self, state, link):
        self._state = state
        self._link = link

    def __iter__(self):
        return self

    def __next__(self):
        link = self._link
        if link is None:
            raise StopIteration
        if link[1] is None:
            state = self._state
            if state.busy:
                raise RuntimeError("cannot re-enter the tee iterator")
            state.busy = True
            try:
                value = next(state.it)
            except StopIteration:
                self._link = None
                raise
            finally:
                state.busy = False
            link[0] = value
            link[1] = [None, None]
        value, self._link = link
        return value


def tee(iterable, n=2):
    if n < 0:
        raise ValueError("n must be >= 0")
    it = iter(iterable)
    state = _TeeState(it)
    link = [None, None]
    return tuple(_TeeIter(state, link) for _ in range(n))


def zip_longest(*iterables, fillvalue=None):
    iters = [iter(it) for it in iterables]
    sentinel = object()
    while True:
        result = []
        active = False
        for it in iters:
            try:
                value = next(it)
                active = True
            except StopIteration:
                value = fillvalue
            result.append(value)
        if not active:
            return
        yield tuple(result)


def product(*iterables, repeat=1):
    pools = [list(it) for it in iterables] * repeat
    result = [[]]
    for pool in pools:
        new_result = []
        for prefix in result:
            for item in pool:
                new_result.append(prefix + [item])
        result = new_result
    for combo in result:
        yield tuple(combo)


def _take(pool, indices, r):
    out = []
    for k in indices[:r]:
        out.append(pool[k])
    return tuple(out)


def permutations(iterable, r=None):
    pool = list(iterable)
    n = len(pool)
    if r is None:
        r = n
    if r > n:
        return
    indices = list(range(n))
    cycles = []
    i = n
    while i > n - r:
        cycles.append(i)
        i -= 1
    yield _take(pool, indices, r)
    while n:
        moved = False
        i = r - 1
        while i >= 0:
            cycles[i] -= 1
            if cycles[i] == 0:
                indices[i:] = indices[i + 1:] + [indices[i]]
                cycles[i] = n - i
            else:
                j = cycles[i]
                indices[i], indices[-j] = indices[-j], indices[i]
                yield _take(pool, indices, r)
                moved = True
                break
            i -= 1
        if not moved:
            return


def combinations(iterable, r):
    pool = list(iterable)
    n = len(pool)
    if r > n:
        return
    indices = list(range(r))
    yield _take(pool, indices, r)
    while True:
        i = r - 1
        while i >= 0 and indices[i] == i + n - r:
            i -= 1
        if i < 0:
            return
        indices[i] += 1
        j = i + 1
        while j < r:
            indices[j] = indices[j - 1] + 1
            j += 1
        yield _take(pool, indices, r)


def combinations_with_replacement(iterable, r):
    pool = list(iterable)
    n = len(pool)
    if not n and r:
        return
    indices = [0] * r
    yield _take(pool, indices, r)
    while True:
        i = r - 1
        while i >= 0 and indices[i] == n - 1:
            i -= 1
        if i < 0:
            return
        indices[i:] = [indices[i] + 1] * (r - i)
        yield _take(pool, indices, r)


def pairwise(iterable):
    it = iter(iterable)
    try:
        prev = next(it)
    except StopIteration:
        return
    for current in it:
        yield (prev, current)
        prev = current


def batched(iterable, n, *, strict=False):
    """Yield successive ``n``-sized batches from *iterable* (PEP 711 /
    new in CPython 3.12). When ``strict=True`` raises ``ValueError``
    if the final batch is shorter than ``n``."""
    if n < 1:
        raise ValueError("n must be at least one")
    it = iter(iterable)
    while True:
        batch = []
        for _ in range(n):
            try:
                batch.append(next(it))
            except StopIteration:
                break
        if not batch:
            return
        if strict and len(batch) != n:
            raise ValueError("batched(): incomplete batch")
        yield tuple(batch)
