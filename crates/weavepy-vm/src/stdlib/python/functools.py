"""WeavePy's pure-Python ``functools`` module.

Covers the high-traffic surface area: ``reduce``, ``partial``,
``lru_cache``, ``cache``, ``wraps``, ``cmp_to_key``, and friends.
"""

__all__ = [
    "reduce",
    "partial",
    "partialmethod",
    "lru_cache",
    "cache",
    "wraps",
    "update_wrapper",
    "cmp_to_key",
]


WRAPPER_ASSIGNMENTS = (
    "__module__",
    "__name__",
    "__qualname__",
    "__doc__",
    "__dict__",
)
WRAPPER_UPDATES = ("__dict__",)


def reduce(function, iterable, *initial):
    it = iter(iterable)
    if initial:
        value = initial[0]
    else:
        try:
            value = next(it)
        except StopIteration:
            raise TypeError(
                "reduce() of empty iterable with no initial value"
            )
    for item in it:
        value = function(value, item)
    return value


class partial:
    """Callable that pre-applies positional and keyword arguments."""

    def __init__(self, func, *args, **kwargs):
        if isinstance(func, partial):
            args = func.args + args
            new_kwargs = dict(func.keywords)
            new_kwargs.update(kwargs)
            kwargs = new_kwargs
            func = func.func
        self.func = func
        self.args = args
        self.keywords = kwargs

    def __call__(self, *args, **kwargs):
        merged = dict(self.keywords)
        merged.update(kwargs)
        return self.func(*self.args, *args, **merged)

    def __repr__(self):
        parts = [repr(self.func)]
        for a in self.args:
            parts.append(repr(a))
        for k, v in self.keywords.items():
            parts.append(k + "=" + repr(v))
        return "functools.partial(" + ", ".join(parts) + ")"


class partialmethod:
    """Descriptor form of :class:`partial` for methods."""

    def __init__(self, func, *args, **kwargs):
        self.func = func
        self.args = args
        self.keywords = kwargs

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        def bound(*args, **kwargs):
            merged = dict(self.keywords)
            merged.update(kwargs)
            return self.func(instance, *self.args, *args, **merged)
        return bound


def update_wrapper(
    wrapper,
    wrapped,
    assigned=WRAPPER_ASSIGNMENTS,
    updated=WRAPPER_UPDATES,
):
    for attr in assigned:
        try:
            value = getattr(wrapped, attr)
        except AttributeError:
            pass
        else:
            try:
                setattr(wrapper, attr, value)
            except (AttributeError, TypeError):
                pass
    for attr in updated:
        try:
            getattr(wrapper, attr).update(getattr(wrapped, attr, {}))
        except (AttributeError, TypeError):
            pass
    wrapper.__wrapped__ = wrapped
    return wrapper


def wraps(wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    def decorator(wrapper):
        return update_wrapper(wrapper, wrapped, assigned, updated)
    return decorator


def lru_cache(maxsize=128, typed=False):
    """Least-recently-used caching decorator."""

    if callable(maxsize):
        # @lru_cache without parentheses
        func = maxsize
        return _make_lru(func, 128, False)

    def decorator(func):
        return _make_lru(func, maxsize, typed)

    return decorator


def cache(func):
    """Unbounded cache decorator (alias for ``lru_cache(maxsize=None)``)."""
    return _make_lru(func, None, False)


class _LruCacheWrapper:
    """Class-based wrapper so we can hang `cache_clear` / `cache_info`
    off the cached callable. WeavePy's Python functions don't yet
    accept arbitrary attribute assignment, so a class is the cleanest
    workaround that still keeps the lookup cost minimal."""

    def __init__(self, func, maxsize, typed):
        self.__wrapped__ = func
        self._maxsize = maxsize
        self._typed = typed
        self._storage = {}
        self._order = []
        self._hits = 0
        self._misses = 0

    def _make_key(self, args, kwargs):
        key = args
        if kwargs:
            key = key + ("__kw__",) + tuple(sorted(kwargs.items()))
        if self._typed:
            key = key + tuple(type(a) for a in args)
        return key

    def __call__(self, *args, **kwargs):
        key = self._make_key(args, kwargs)
        if key in self._storage:
            self._hits += 1
            self._order.remove(key)
            self._order.append(key)
            return self._storage[key]
        self._misses += 1
        value = self.__wrapped__(*args, **kwargs)
        self._storage[key] = value
        self._order.append(key)
        if self._maxsize is not None and len(self._order) > self._maxsize:
            old = self._order.pop(0)
            del self._storage[old]
        return value

    def cache_clear(self):
        self._storage.clear()
        self._order.clear()
        self._hits = 0
        self._misses = 0

    def cache_info(self):
        return (self._hits, self._misses, self._maxsize, len(self._storage))


def _make_lru(func, maxsize, typed):
    return _LruCacheWrapper(func, maxsize, typed)


def cmp_to_key(cmp):
    """Convert an old-style comparison function into a key function."""

    class K:
        __slots__ = ("obj",)

        def __init__(self, obj):
            self.obj = obj

        def __lt__(self, other):
            return cmp(self.obj, other.obj) < 0

        def __le__(self, other):
            return cmp(self.obj, other.obj) <= 0

        def __gt__(self, other):
            return cmp(self.obj, other.obj) > 0

        def __ge__(self, other):
            return cmp(self.obj, other.obj) >= 0

        def __eq__(self, other):
            return cmp(self.obj, other.obj) == 0

        def __ne__(self, other):
            return cmp(self.obj, other.obj) != 0

    return K
