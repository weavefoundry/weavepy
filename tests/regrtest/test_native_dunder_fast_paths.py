"""Constructors with a Python __new__, instance operators over built-in
operands, pure comparison methods, class attributes read through
instances, and unset __slots__ members, each run hot enough that the
core loop's and compiled code's fast paths take them, with the results
the general protocol gives."""

N = 3000


# A Python __new__: __init__ runs on an instance of the class (or a
# subclass), not on anything else, and raises propagate.
log = []


class A:
    def __new__(cls, x):
        return object.__new__(cls)

    def __init__(self, x):
        self.x = x


class B(A):
    pass


class Other:
    def __new__(cls, x):
        return 42


class Sub:
    def __new__(cls, x):
        return object.__new__(SubSub)

    def __init__(self, x):
        self.x = ("sub", x)


class SubSub(Sub):
    def __init__(self, x):
        self.x = ("subsub", x)


class InitRet:
    def __new__(cls, x):
        return object.__new__(cls)

    def __init__(self, x):
        return 5


class Slots:
    __slots__ = ("x", "y")

    def __new__(cls, x, y=7):
        return object.__new__(cls)

    def __init__(self, x, y=7):
        self.x = x
        self.y = y


for i in range(N):
    a, b = A(i), B(i)
    assert (type(a), a.x, type(b), b.x) == (A, i, B, i)
    assert Other(i) == 42
    s = Sub(i)
    assert type(s) is SubSub and s.x == ("subsub", i)
    try:
        InitRet(i)
    except TypeError as e:
        assert str(e) == "__init__() should return None, not 'int'"
    else:
        raise AssertionError("InitRet")
    sl = Slots(i)
    assert (sl.x, sl.y) == (i, 7)
    assert A(*(i,)).x == i


# An instance's operator over a built-in operand: the instance's method
# first, NotImplemented a TypeError naming both operand types.
class P:
    def __init__(self, v):
        self.v = v

    def __truediv__(self, k):
        if isinstance(k, str):
            return P(self.v + "/" + k)
        return NotImplemented

    def __rtruediv__(self, k):
        return P(k + "|" + self.v)

    def __add__(self, o):
        return self.v + str(o) if isinstance(o, int) else NotImplemented

    def __mul__(self, o):
        return "mul"


p = P("a")
for i in range(N):
    assert (p / "b").v == "a/b"
    assert p + i == "a" + str(i)
    assert p * 3 == "mul"
    assert ("c" / p).v == "c|a"
    try:
        p / 5
    except TypeError as e:
        assert str(e) == "unsupported operand type(s) for /: 'P' and 'int'", e
    else:
        raise AssertionError("p / 5")
    try:
        p + "s"
    except TypeError as e:
        assert str(e) == "unsupported operand type(s) for +: 'P' and 'str'", e
    else:
        raise AssertionError("p + 's'")


# A pure comparison method: a subclass's reflected method first, and
# NotImplemented falling back to the other operand and identity.
class C:
    def __init__(self, x):
        self.x = x

    def __eq__(self, other):
        return self.x == other.x

    def __lt__(self, other):
        return self.x < other.x


class D(C):
    def __gt__(self, other):
        return "D-gt"


class M:
    def __init__(self, x):
        self.x = x

    def __eq__(self, other):
        if not isinstance(other, M):
            return NotImplemented
        return self.x == other.x


c1, c2, d, m = C(1), C(2), D(1), M(1)
for i in range(N):
    assert (c1 == c2) is False and (c1 == C(1)) is True
    assert (c1 < c2) is True
    assert (c1 < d) == "D-gt"
    assert (m == 1) is False and (m != 1) is True and (m == M(1)) is True
    try:
        c1 == 5
    except AttributeError as e:
        assert str(e) == "'int' object has no attribute 'x'"
    else:
        raise AssertionError("c1 == 5")


# A class attribute read through an instance until the instance, or the
# class, changes it.
class K:
    V = 5

    def __init__(self):
        self.y = 1


k = K()
seen = []
for i in range(N):
    seen.append(k.V)
    if i == 1000:
        k.V = 99
    if i == 2000:
        del k.V
        K.V = 7
assert seen[1000] == 5 and seen[1001] == 99 and seen[2001] == 7


# An unset __slots__ member: the AttributeError names the class by its
# qualified name, getattr() takes its default, hasattr() says no.
class Outer:
    class S:
        __slots__ = ("a", "b")


def unset_slots():
    for i in range(N):
        o = Outer.S()
        try:
            o.a
        except AttributeError as e:
            assert str(e) == "'Outer.S' object has no attribute 'a'", str(e)
            assert e.name == "a" and e.obj is o
        else:
            raise AssertionError("unset slot")
        assert getattr(o, "b", "dflt") == "dflt"
        assert not hasattr(o, "a")
        o.a = i
        assert o.a == i


unset_slots()

print("native dunder fast paths ok")
