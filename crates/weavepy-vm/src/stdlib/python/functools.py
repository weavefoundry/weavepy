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
    "total_ordering",
    "singledispatch",
    "cached_property",
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
    """Callable that pre-applies positional and keyword arguments.

    `func` is positional-only (CPython's `partial.__new__(cls, func, /,
    *args, **keywords)`): without that, a keyword named `func`/`self`
    passed through to the wrapped callable — e.g. `operator.methodcaller`'s
    pickle reduce builds `partial(methodcaller, name, self=..., name=...)`
    — would collide with the constructor's own parameter and raise
    "got multiple values for argument 'self'".
    """

    def __new__(cls, func, /, *args, **keywords):
        if not callable(func):
            raise TypeError("the first argument must be callable")
        if isinstance(func, partial):
            args = func.args + args
            keywords = {**func.keywords, **keywords}
            func = func.func
        self = super(partial, cls).__new__(cls)
        self.func = func
        self.args = args
        self.keywords = keywords
        return self

    def __call__(self, /, *args, **keywords):
        keywords = {**self.keywords, **keywords}
        return self.func(*self.args, *args, **keywords)

    def __repr__(self):
        qualname = type(self).__qualname__
        module = type(self).__module__ or "functools"
        parts = [repr(self.func)]
        for a in self.args:
            parts.append(repr(a))
        for k, v in self.keywords.items():
            parts.append(k + "=" + repr(v))
        return module + "." + qualname + "(" + ", ".join(parts) + ")"

    def __reduce__(self):
        return (
            type(self),
            (self.func,),
            (self.func, self.args, self.keywords or None, self.__dict__ or None),
        )

    def __setstate__(self, state):
        if not isinstance(state, tuple):
            raise TypeError("argument to __setstate__ must be a tuple")
        if len(state) != 4:
            raise TypeError("expected 4 items in state, got %d" % len(state))
        func, args, kwds, namespace = state
        if (not callable(func) or not isinstance(args, tuple) or
                (kwds is not None and not isinstance(kwds, dict)) or
                (namespace is not None and not isinstance(namespace, dict))):
            raise TypeError("invalid partial state")
        args = tuple(args)
        if kwds is None:
            kwds = {}
        elif type(kwds) is not dict:
            kwds = dict(kwds)
        if namespace is None:
            namespace = {}
        self.__dict__ = namespace
        self.func = func
        self.args = args
        self.keywords = kwds


class partialmethod:
    """Descriptor form of :class:`partial` for methods."""

    def __init__(self, func, /, *args, **keywords):
        # `func` is positional-only (PEP 570) so a wrapped callable may itself
        # take `self`/`func` keyword arguments without colliding — matches
        # CPython's `partialmethod.__init__` signature.
        if isinstance(func, partialmethod):
            # Flatten nested partialmethods so cls/self stay ahead of all
            # other arguments and only one underlying call happens.
            self.func = func.func
            self.args = func.args + args
            self.keywords = {**func.keywords, **keywords}
        else:
            self.func = func
            self.args = args
            self.keywords = keywords

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
        # CPython exposes ``cache_info`` as a named tuple with the
        # ``hits``, ``misses``, ``maxsize``, ``currsize`` fields so
        # callers can use attribute access (``info.hits``).
        return _CacheInfo(
            hits=self._hits,
            misses=self._misses,
            maxsize=self._maxsize,
            currsize=len(self._storage),
        )


class _CacheInfo:
    """Lightweight stand-in for ``collections.namedtuple`` that gives
    ``functools.lru_cache.cache_info`` its CPython-compatible
    attribute access plus tuple-style iteration. Kept local so the
    real ``collections.namedtuple`` import isn't required."""

    __slots__ = ("hits", "misses", "maxsize", "currsize")

    def __init__(self, hits=0, misses=0, maxsize=None, currsize=0):
        self.hits = hits
        self.misses = misses
        self.maxsize = maxsize
        self.currsize = currsize

    def __iter__(self):
        return iter((self.hits, self.misses, self.maxsize, self.currsize))

    def __eq__(self, other):
        if isinstance(other, _CacheInfo):
            return (
                self.hits == other.hits
                and self.misses == other.misses
                and self.maxsize == other.maxsize
                and self.currsize == other.currsize
            )
        if isinstance(other, tuple):
            return (self.hits, self.misses, self.maxsize, self.currsize) == other
        return NotImplemented

    def __repr__(self):
        return (
            f"CacheInfo(hits={self.hits}, misses={self.misses}, "
            f"maxsize={self.maxsize}, currsize={self.currsize})"
        )


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


# ---- total_ordering ----------------------------------------------------------
# Verbatim CPython 3.13: fills in the missing rich-comparison methods from a
# single defined one (RFC 0037 WS8 functools edges).

def _gt_from_lt(self, other, NotImplemented=NotImplemented):
    'Return a > b.  Computed by @total_ordering from (not a < b) and (a != b).'
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result and self != other

def _le_from_lt(self, other, NotImplemented=NotImplemented):
    'Return a <= b.  Computed by @total_ordering from (a < b) or (a == b).'
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result or self == other

def _ge_from_lt(self, other, NotImplemented=NotImplemented):
    'Return a >= b.  Computed by @total_ordering from (not a < b).'
    op_result = type(self).__lt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result

def _ge_from_le(self, other, NotImplemented=NotImplemented):
    'Return a >= b.  Computed by @total_ordering from (not a <= b) or (a == b).'
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result or self == other

def _lt_from_le(self, other, NotImplemented=NotImplemented):
    'Return a < b.  Computed by @total_ordering from (a <= b) and (a != b).'
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result and self != other

def _gt_from_le(self, other, NotImplemented=NotImplemented):
    'Return a > b.  Computed by @total_ordering from (not a <= b).'
    op_result = type(self).__le__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result

def _lt_from_gt(self, other, NotImplemented=NotImplemented):
    'Return a < b.  Computed by @total_ordering from (not a > b) and (a != b).'
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result and self != other

def _ge_from_gt(self, other, NotImplemented=NotImplemented):
    'Return a >= b.  Computed by @total_ordering from (a > b) or (a == b).'
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result or self == other

def _le_from_gt(self, other, NotImplemented=NotImplemented):
    'Return a <= b.  Computed by @total_ordering from (not a > b).'
    op_result = type(self).__gt__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result

def _le_from_ge(self, other, NotImplemented=NotImplemented):
    'Return a <= b.  Computed by @total_ordering from (not a >= b) or (a == b).'
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result or self == other

def _gt_from_ge(self, other, NotImplemented=NotImplemented):
    'Return a > b.  Computed by @total_ordering from (a >= b) and (a != b).'
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return op_result and self != other

def _lt_from_ge(self, other, NotImplemented=NotImplemented):
    'Return a < b.  Computed by @total_ordering from (not a >= b).'
    op_result = type(self).__ge__(self, other)
    if op_result is NotImplemented:
        return op_result
    return not op_result

_convert = {
    '__lt__': [('__gt__', _gt_from_lt),
               ('__le__', _le_from_lt),
               ('__ge__', _ge_from_lt)],
    '__le__': [('__ge__', _ge_from_le),
               ('__lt__', _lt_from_le),
               ('__gt__', _gt_from_le)],
    '__gt__': [('__lt__', _lt_from_gt),
               ('__ge__', _ge_from_gt),
               ('__le__', _le_from_gt)],
    '__ge__': [('__le__', _le_from_ge),
               ('__gt__', _gt_from_ge),
               ('__lt__', _lt_from_ge)],
}

def total_ordering(cls):
    """Class decorator that fills in missing ordering methods"""
    # Find user-defined comparisons (not those inherited from object).
    roots = {op for op in _convert if getattr(cls, op, None) is not getattr(object, op, None)}
    if not roots:
        raise ValueError('must define at least one ordering operation: < > <= >=')
    root = max(roots)       # prefer __lt__ to __le__ to __gt__ to __ge__
    for opname, opfunc in _convert[root]:
        if opname not in roots:
            opfunc.__name__ = opname
            setattr(cls, opname, opfunc)
    return cls


# ---- single-dispatch generic functions --------------------------------------


class _SingleDispatchCallable:
    """Backing object for :func:`singledispatch`.

    Implementing this as a class (instead of nested closures) keeps
    the registry visible to ``register``'s inner decorator without
    relying on three-level freevar passthrough.
    """

    def __init__(self, func):
        self._default = func
        self.registry = {object: func}
        self.__wrapped__ = func

    def dispatch(self, cls):
        for base in cls.__mro__:
            if base in self.registry:
                return self.registry[base]
        return self._default

    def register(self, cls, impl=None):
        if impl is None:
            outer_self = self
            outer_cls = cls

            def decorator(real_impl):
                outer_self.registry[outer_cls] = real_impl
                return real_impl

            return decorator
        self.registry[cls] = impl
        return impl

    def __call__(self, *args, **kwargs):
        if not args:
            raise TypeError(
                "singledispatch function requires at least one positional argument"
            )
        impl = self.dispatch(type(args[0]))
        return impl(*args, **kwargs)


def singledispatch(func):
    """Single-dispatch generic-function decorator.

    Mirrors :func:`functools.singledispatch`. Subsequent calls to the
    returned callable dispatch on the *runtime* type of the first
    argument; alternative implementations are registered with
    ``@my_func.register(type)``.

    Notes:
    - We don't honour the C-extension's caching of resolved types;
      the linear walk over registered types is fast enough for our
      target workloads.
    - PEP 585 / annotation-based registration is omitted because we
      don't yet have a stable ``get_type_hints`` story for module-
      level functions defined in WeavePy.
    """
    return _SingleDispatchCallable(func)


# ---- cached_property --------------------------------------------------------


_MISSING = object()


class cached_property:
    """Method decorator turning ``self.foo`` into a once-computed attr.

    Compared to :class:`property`, the value produced by the wrapped
    function is stored back onto the instance's ``__dict__`` under the
    attribute's name, so subsequent accesses short-circuit the
    descriptor and don't re-enter the wrapped function.
    """

    def __init__(self, func):
        self.func = func
        self.attrname = None
        self.__doc__ = getattr(func, "__doc__", None)

    def __set_name__(self, owner, name):
        if self.attrname is None:
            self.attrname = name
        elif name != self.attrname:
            raise TypeError(
                "Cannot assign the same cached_property to two different names"
            )

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        if self.attrname is None:
            raise TypeError(
                "Cannot use cached_property instance without calling __set_name__"
            )
        cached = instance.__dict__.get(self.attrname, _MISSING)
        if cached is _MISSING:
            cached = self.func(instance)
            instance.__dict__[self.attrname] = cached
        return cached
