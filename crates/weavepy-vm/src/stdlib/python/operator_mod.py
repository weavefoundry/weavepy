"""Operator interface — WeavePy port of CPython's ``operator``.

Provides function form of the standard operators (``operator.add``
etc.) plus the higher-order ``itemgetter``, ``attrgetter`` and
``methodcaller`` helpers.
"""


def lt(a, b):
    return a < b


def le(a, b):
    return a <= b


def eq(a, b):
    return a == b


def ne(a, b):
    return a != b


def gt(a, b):
    return a > b


def ge(a, b):
    return a >= b


__lt__ = lt
__le__ = le
__eq__ = eq
__ne__ = ne
__gt__ = gt
__ge__ = ge


def not_(a):
    return not a


def truth(a):
    return bool(a)


def is_(a, b):
    return a is b


def is_not(a, b):
    return a is not b


def abs(a):
    import builtins
    return builtins.abs(a)


def add(a, b):
    return a + b


def and_(a, b):
    return a & b


def floordiv(a, b):
    return a // b


def index(a):
    if hasattr(a, "__index__"):
        return a.__index__()
    raise TypeError("object is not indexable")


def inv(a):
    return ~a


def invert(a):
    return ~a


def lshift(a, b):
    return a << b


def mod(a, b):
    return a % b


def mul(a, b):
    return a * b


def matmul(a, b):
    return a @ b


def neg(a):
    return -a


def or_(a, b):
    return a | b


def pos(a):
    return +a


def pow(a, b):
    import builtins
    return builtins.pow(a, b)


def rshift(a, b):
    return a >> b


def sub(a, b):
    return a - b


def truediv(a, b):
    return a / b


def xor(a, b):
    return a ^ b


def concat(a, b):
    if hasattr(a, "__add__"):
        return a + b
    raise TypeError("concat: not a sequence")


def contains(a, b):
    return b in a


def countOf(a, b):
    n = 0
    for x in a:
        if x == b:
            n += 1
    return n


def indexOf(a, b):
    for i, x in enumerate(a):
        if x == b:
            return i
    raise ValueError("indexOf(a, b): b not in a")


def getitem(a, b):
    return a[b]


def setitem(a, b, c):
    a[b] = c


def delitem(a, b):
    del a[b]


def length_hint(obj, default=0):
    try:
        return obj.__length_hint__()
    except AttributeError:
        return default


class attrgetter:
    """Return a callable that fetches attribute(s) from an object."""

    __slots__ = ("_attrs", "_call")

    def __init__(self, attr, *more):
        attrs = (attr,) + more
        for a in attrs:
            if not isinstance(a, str):
                raise TypeError("attribute name must be str")
        self._attrs = attrs

        def resolve(obj, name):
            for part in name.split("."):
                obj = getattr(obj, part)
            return obj

        if len(attrs) == 1:
            self._call = lambda obj: resolve(obj, attrs[0])
        else:
            self._call = lambda obj: tuple(resolve(obj, a) for a in attrs)

    def __call__(self, obj):
        return self._call(obj)

    def __repr__(self):
        return "operator.attrgetter({})".format(", ".join(repr(a) for a in self._attrs))


class itemgetter:
    """Return a callable that fetches the given item(s) from a sequence."""

    __slots__ = ("_items",)

    def __init__(self, item, *more):
        self._items = (item,) + more

    def __call__(self, obj):
        if len(self._items) == 1:
            return obj[self._items[0]]
        return tuple(obj[i] for i in self._items)

    def __repr__(self):
        return "operator.itemgetter({})".format(", ".join(repr(i) for i in self._items))


class methodcaller:
    """Return a callable that invokes ``name`` on its argument."""

    __slots__ = ("_name", "_args", "_kwargs")

    def __init__(self, name, *args, **kwargs):
        self._name = name
        self._args = args
        self._kwargs = kwargs

    def __call__(self, obj):
        return getattr(obj, self._name)(*self._args, **self._kwargs)

    def __repr__(self):
        parts = [repr(self._name)]
        parts.extend(repr(a) for a in self._args)
        for k, v in self._kwargs.items():
            parts.append("{}={!r}".format(k, v))
        return "operator.methodcaller({})".format(", ".join(parts))


def iadd(a, b):
    a += b
    return a


def iand(a, b):
    a &= b
    return a


def iconcat(a, b):
    a += b
    return a


def ifloordiv(a, b):
    a //= b
    return a


def ilshift(a, b):
    a <<= b
    return a


def imod(a, b):
    a %= b
    return a


def imul(a, b):
    a *= b
    return a


def imatmul(a, b):
    a @= b
    return a


def ior(a, b):
    a |= b
    return a


def ipow(a, b):
    a **= b
    return a


def irshift(a, b):
    a >>= b
    return a


def isub(a, b):
    a -= b
    return a


def itruediv(a, b):
    a /= b
    return a


def ixor(a, b):
    a ^= b
    return a


__all__ = [
    "lt", "le", "eq", "ne", "gt", "ge",
    "not_", "truth", "is_", "is_not",
    "abs", "add", "and_", "floordiv", "index", "inv", "invert",
    "lshift", "mod", "mul", "matmul", "neg", "or_", "pos", "pow",
    "rshift", "sub", "truediv", "xor",
    "concat", "contains", "countOf", "indexOf",
    "getitem", "setitem", "delitem", "length_hint",
    "attrgetter", "itemgetter", "methodcaller",
    "iadd", "iand", "iconcat", "ifloordiv", "ilshift", "imod",
    "imul", "imatmul", "ior", "ipow", "irshift", "isub",
    "itruediv", "ixor",
]
