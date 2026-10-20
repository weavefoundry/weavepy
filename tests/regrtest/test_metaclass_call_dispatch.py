"""Calling a class whose metaclass defines `__call__` runs that method
with the class as its receiver, in positional and keyword calls, through
inherited, replaced and non-function `__call__`s, and with exceptions
and argument lifetimes intact."""

import enum
import gc


class Meta(type):
    def __call__(cls, *args, **kw):
        return ("meta", cls.__name__, args, sorted(kw.items()))


class A(metaclass=Meta):
    pass


class B(A):
    pass


def call_a(c):
    return c(1, 2)


def call_a_kw(c):
    return c(1, k=2)


for _ in range(300):
    assert call_a(A) == ("meta", "A", (1, 2), [])
    assert call_a(B) == ("meta", "B", (1, 2), [])
    assert call_a_kw(A) == ("meta", "A", (1,), [("k", 2)])


# Replacing the metaclass's `__call__` after the sites warmed up.
def new_call(cls, *args, **kw):
    return ("new", cls.__name__, len(args), len(kw))


Meta.__call__ = new_call
for _ in range(100):
    assert call_a(A) == ("new", "A", 2, 0)
    assert call_a_kw(B) == ("new", "B", 1, 1)
del Meta.__call__
for _ in range(100):
    try:
        call_a(A)
    except TypeError as e:
        assert str(e) == "A() takes no arguments", str(e)
    else:
        raise AssertionError("expected TypeError")


# A `__call__` that is not a plain function is honored as such.
class StaticMeta(type):
    __call__ = staticmethod(lambda *args: ("static", args))


class S(metaclass=StaticMeta):
    pass


class ClassMeta(type):
    @classmethod
    def __call__(mcls, *args):
        return ("classmethod", mcls.__name__, args)


class Cm(metaclass=ClassMeta):
    pass


for _ in range(200):
    assert S(1) == ("static", (1,))
    assert Cm(1) == ("classmethod", "ClassMeta", (1,))


# A metaclass `__call__` delegating to `type.__call__`.
class Counting(type):
    count = 0

    def __call__(cls, *args, **kw):
        Counting.count += 1
        return super().__call__(*args, **kw)


class P(metaclass=Counting):
    def __init__(self, x, y=0):
        self.x, self.y = x, y


for i in range(300):
    p = P(i, y=1) if i % 2 else P(i)
    assert p.x == i and p.y == i % 2
assert Counting.count == 300


# Exceptions raised inside the metaclass `__call__` propagate with it in
# the traceback.
class Raising(type):
    def __call__(cls, *args):
        raise ValueError("no " + cls.__name__)


class R(metaclass=Raising):
    pass


for _ in range(100):
    try:
        R(1)
    except ValueError as e:
        assert str(e) == "no R"
        tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        assert names[-1] == "__call__", names
    else:
        raise AssertionError("expected ValueError")


# Enum lookups by value go through `EnumType.__call__`.
class Color(enum.Enum):
    RED = 1
    GREEN = 2


for _ in range(300):
    assert Color(1) is Color.RED
    assert Color(value=2) is Color.GREEN
try:
    Color(3)
except ValueError as e:
    assert str(e) == "3 is not a valid Color", str(e)
else:
    raise AssertionError("expected ValueError")


# The argument's last reference dies with the call.
class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


class Drop(type):
    def __call__(cls, arg):
        return 1


class D(metaclass=Drop):
    pass


def make_d():
    D(Tracked())
    return Tracked.alive


gc.disable()
try:
    for _ in range(300):
        assert make_d() == 0
finally:
    gc.enable()

print("ok")
