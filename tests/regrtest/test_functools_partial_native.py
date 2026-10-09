"""`functools.partial` construction and calls, and `functools.reduce`.

WeavePy builds the common partial shape natively and calls it without a
Python frame, reading the instance's own fields (as CPython's C type
does) rather than its overridable attributes. Everything else goes
through the pure-Python `__new__`.
"""

import copy
import functools
import operator
import pickle
import weakref
from functools import Placeholder, partial, reduce


def raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc as e:
        return str(e)
    raise AssertionError("%s not raised" % exc.__name__)


def f(*args, **kwargs):
    return args, kwargs


p = partial(f, 1, a=2)
assert type(p) is partial
assert p.func is f and p.args == (1,) and p.keywords == {"a": 2}
assert p(3, b=4) == ((1, 3), {"a": 2, "b": 4})
assert p(a=5) == ((1,), {"a": 5})
assert partial(f)() == ((), {})

# Nesting merges into one partial.
q = partial(partial(f, 1, a=1), 2, b=2)
assert q.func is f and q.args == (1, 2) and q.keywords == {"a": 1, "b": 2}

# Placeholders take the pure-Python path.
assert partial(f, Placeholder, 2)(1, 3) == ((1, 2, 3), {})
raises(TypeError, partial, f, 1, Placeholder)
raises(TypeError, partial, f, a=Placeholder)
raises(TypeError, partial, 1)
assert raises(TypeError, partial) == "type 'partial' takes at least one argument"


class P(partial):
    pass


assert type(P(f, 1)) is P and P(f, 1)(2) == ((1, 2), {})


class Q(partial):
    def __new__(cls, *args, **kwargs):
        self = super().__new__(cls, *args, **kwargs)
        self.extra = 1
        return self


assert Q(f, 1).extra == 1 and Q(f, 1)(2) == ((1, 2), {})


class R(partial):
    @property
    def func(self):
        return print


# The call uses the stored function, not the overridden attribute.
assert R(f, 1)(2) == ((1, 2), {})

p = partial(f)
p.x = 5
assert p.x == 5 and p.__dict__ == {"x": 5}
assert weakref.ref(p)() is p
assert pickle.loads(pickle.dumps(partial(operator.add, 1)))(2) == 3
assert copy.copy(partial(f, 1))(2) == ((1, 2), {})
assert copy.deepcopy(partial(f, [1]))(2) == (([1], 2), {})
p = partial(int)
p.__setstate__((f, (5,), {"a": 1}, None))
assert p(6) == ((5, 6), {"a": 1})
p.__setstate__((f, (Placeholder, 5), None, None))
assert p(6) == ((6, 5), {})


class C:
    m = partial(f, 0)


c = C()
assert c.m(1) == ((0, c, 1), {})
raises(TypeError, partial(lambda x: x), 1, 2)

assert reduce(operator.add, [1, 2, 3]) == 6
assert reduce(operator.add, [], 5) == 5
assert reduce(operator.add, [1], initial=10) == 11
assert reduce(lambda a, b: a * b, (x for x in range(1, 6))) == 120
raises(TypeError, reduce, operator.add, [])
assert raises(TypeError, reduce, operator.add, 5) == "reduce() arg 2 must support iteration"

print("functools partial native ok")
