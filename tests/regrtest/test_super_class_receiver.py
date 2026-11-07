"""`super(C, cls).name` with a class for the receiver (a class method's or
`__new__`'s, as `fractions` builds its values), and an operator between
two instances of one class that also defines the reflected method, both
taken by compiled code in place, keep CPython's results: the attribute
unbound (a function, a static method's, a builtin's `__new__`), a class
method's binding, an override deleted later, and the reflected method
never tried."""
from fractions import Fraction


class Base:
    def f(self):
        return "base f"

    @staticmethod
    def s(x):
        return x + 1

    @classmethod
    def c(cls):
        return cls.__name__


class Mid(Base):
    def __new__(cls, v):
        obj = super(Mid, cls).__new__(cls)
        obj.v = v
        return obj

    @classmethod
    def make(cls, v):
        return (super(Mid, cls).f, super(Mid, cls).s(v), super(Mid, cls).c())


class Leaf(Mid):
    pass


class Num:
    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        if isinstance(other, Num):
            return Num(self.v + other.v)
        return NotImplemented

    def __radd__(self, other):
        return "radd"

    def __sub__(self, other):
        return NotImplemented

    def __rsub__(self, other):
        return "rsub"


class MyInt(int):
    def __new__(cls, v):
        return super(MyInt, cls).__new__(cls, v * 2)


def run(i):
    m = Leaf(i)
    assert type(m) is Leaf and m.v == i
    f, s, c = Leaf.make(i)
    assert f is Base.f and s == i + 1 and c == "Leaf"
    assert MyInt(i) == 2 * i and type(MyInt(i)) is MyInt
    assert (Num(i) + Num(2)).v == i + 2 and 3 + Num(1) == "radd"
    try:
        Num(1) - Num(2)
    except TypeError as e:
        assert "unsupported operand type(s) for -: 'Num' and 'Num'" in str(e), e
    else:
        raise AssertionError("a same-class reflected method ran")
    a, b = Fraction(i, 3), Fraction(2, 7)
    assert a + b == Fraction(7 * i + 6, 21) and (a + b).denominator in (21, 7, 3, 1)
    assert a * b == Fraction(2 * i, 21) and Fraction(i) == i


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
# An override deleted: the next class along answers.
del Base.s
try:
    Leaf.make(1)
except AttributeError:
    pass
else:
    raise AssertionError("a deleted static method was still found")
print("ok")
