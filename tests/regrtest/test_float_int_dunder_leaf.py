"""`float(x)` and `int(x)` of instances whose `__float__` or `__int__` is
a Python function, in compiled code.

A pure body is evaluated in place, `int / int` among it when the
operands fit a double exactly; operands past `2**53`, a zero divisor, a
`__float__` answering an `int`, an `__int__` answering a `bool`, and a
class with only `__index__` take the full call, with CPython's results
and errors. Run it past the compile threshold.
"""

from fractions import Fraction


class R:
    def __init__(self, a, b):
        self.a, self.b = a, b

    def __float__(self):
        return self.a / self.b

    def __int__(self):
        return self.a // self.b


class Wrong:
    def __float__(self):
        return 1

    def __int__(self):
        return True


class OnlyIndex:
    def __index__(self):
        return 7


def conv(x):
    return float(x), int(x)


big = 2**60 + 1
for i in range(3000):
    assert conv(R(7, 2)) == (3.5, 3)
    assert conv(R(-7, 2)) == (-3.5, -4)
    assert float(R(1, 3)) == 1 / 3
    assert float(R(big, 3)) == big / 3
    assert float(R(2**53, 2**53 - 1)) == 2**53 / (2**53 - 1)
    try:
        float(R(1, 0))
    except ZeroDivisionError:
        pass
    else:
        raise AssertionError("no raise")
    try:
        float(Wrong())
    except TypeError:
        pass
    else:
        raise AssertionError("no raise")
    assert int(OnlyIndex()) == 7 and float(OnlyIndex()) == 7.0
    assert float(Fraction(i + 1, 7)) == (i + 1) / 7
import warnings
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    assert int(Wrong()) == 1 and type(int(Wrong())) is int
