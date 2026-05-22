"""Public ``fractions`` module (RFC 0019).

Implements rational arithmetic on top of WeavePy's arbitrary-precision
``int`` type. The surface mirrors CPython's :class:`fractions.Fraction`
for the universally-used operations: parsing, arithmetic, comparisons,
``limit_denominator``, ``__hash__``, and ``as_integer_ratio``.
"""

import math
import re

_PATTERN = re.compile(
    r"\A\s*"
    r"(?P<sign>[-+])?"
    r"(?P<num>\d+)"
    r"(?:/(?P<denom>\d+))?"
    r"\s*\Z"
)


def _gcd(a, b):
    if a < 0:
        a = -a
    if b < 0:
        b = -b
    while b:
        a, b = b, a % b
    return a


def _normalize(num, den):
    if den == 0:
        raise ZeroDivisionError("Fraction(%d, 0)" % num)
    if den < 0:
        num, den = -num, -den
    g = _gcd(num, den)
    if g != 1:
        num //= g
        den //= g
    return num, den


class Fraction:
    """Rational number represented as ``num/den``."""

    __slots__ = ("_numerator", "_denominator")

    def __new__(cls, numerator=0, denominator=None, *, _normalize=True):
        self = object.__new__(cls)
        if denominator is None:
            if isinstance(numerator, int):
                self._numerator = numerator
                self._denominator = 1
                return self
            if isinstance(numerator, float):
                if not math.isfinite(numerator):
                    raise OverflowError(
                        "cannot convert non-finite float to Fraction")
                num, den = numerator.as_integer_ratio()
                self._numerator = num
                self._denominator = den
                return self
            if isinstance(numerator, str):
                m = _PATTERN.match(numerator)
                if not m:
                    raise ValueError("Invalid fraction literal: %r" % numerator)
                num = int(m.group("num"))
                if m.group("denom"):
                    den = int(m.group("denom"))
                else:
                    den = 1
                if m.group("sign") == "-":
                    num = -num
                num, den = _norm(num, den)
                self._numerator = num
                self._denominator = den
                return self
            if isinstance(numerator, Fraction):
                self._numerator = numerator._numerator
                self._denominator = numerator._denominator
                return self
            raise TypeError(
                "argument should be a string or a number, not %r" %
                type(numerator).__name__)
        if not isinstance(numerator, int) or not isinstance(denominator, int):
            raise TypeError("both numerator and denominator must be int")
        num, den = _norm(numerator, denominator)
        self._numerator = num
        self._denominator = den
        return self

    # -- properties ------------------------------------------------

    @property
    def numerator(self):
        return self._numerator

    @property
    def denominator(self):
        return self._denominator

    # -- conversions ----------------------------------------------

    def __repr__(self):
        return "Fraction(%d, %d)" % (self._numerator, self._denominator)

    def __str__(self):
        if self._denominator == 1:
            return str(self._numerator)
        return "%d/%d" % (self._numerator, self._denominator)

    def __float__(self):
        return self._numerator / self._denominator

    def __int__(self):
        if self._numerator < 0:
            return -(-self._numerator // self._denominator)
        return self._numerator // self._denominator

    def __bool__(self):
        return self._numerator != 0

    def __hash__(self):
        return hash((self._numerator, self._denominator))

    def as_integer_ratio(self):
        return (self._numerator, self._denominator)

    # -- arithmetic ------------------------------------------------

    def _coerce(self, other):
        if isinstance(other, Fraction):
            return other
        if isinstance(other, int):
            return Fraction(other, 1)
        if isinstance(other, float):
            return Fraction(other)
        return NotImplemented

    def __add__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        n = self._numerator * o._denominator + o._numerator * self._denominator
        d = self._denominator * o._denominator
        return Fraction(n, d)

    __radd__ = __add__

    def __sub__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        n = self._numerator * o._denominator - o._numerator * self._denominator
        d = self._denominator * o._denominator
        return Fraction(n, d)

    def __rsub__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        return o.__sub__(self)

    def __mul__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        return Fraction(self._numerator * o._numerator,
                        self._denominator * o._denominator)

    __rmul__ = __mul__

    def __truediv__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        if o._numerator == 0:
            raise ZeroDivisionError("Fraction(%d, 0)" % self._numerator)
        return Fraction(self._numerator * o._denominator,
                        self._denominator * o._numerator)

    def __rtruediv__(self, other):
        o = self._coerce(other)
        if o is NotImplemented:
            return NotImplemented
        return o.__truediv__(self)

    def __neg__(self):
        return Fraction(-self._numerator, self._denominator)

    def __pos__(self):
        return self

    def __abs__(self):
        return Fraction(abs(self._numerator), self._denominator)

    def __pow__(self, exp):
        if isinstance(exp, int):
            if exp >= 0:
                return Fraction(self._numerator ** exp,
                                self._denominator ** exp)
            else:
                if self._numerator == 0:
                    raise ZeroDivisionError("0 ** negative")
                return Fraction(self._denominator ** -exp,
                                self._numerator ** -exp)
        if isinstance(exp, (Fraction, float)):
            return float(self) ** float(exp)
        return NotImplemented

    # -- comparisons ----------------------------------------------

    def _cmp(self, other):
        if isinstance(other, Fraction):
            a = self._numerator * other._denominator
            b = other._numerator * self._denominator
            return (a > b) - (a < b)
        if isinstance(other, int):
            return self._cmp(Fraction(other))
        if isinstance(other, float):
            if math.isfinite(other):
                return self._cmp(Fraction(other))
            return -1 if other > 0 else 1
        return NotImplemented

    def __eq__(self, other):
        if isinstance(other, Fraction):
            return (self._numerator == other._numerator and
                    self._denominator == other._denominator)
        if isinstance(other, int):
            return self._numerator == other and self._denominator == 1
        if isinstance(other, float):
            if math.isfinite(other):
                return self == Fraction(other)
            return False
        return NotImplemented

    def __lt__(self, other):
        c = self._cmp(other)
        if c is NotImplemented:
            return NotImplemented
        return c < 0

    def __le__(self, other):
        c = self._cmp(other)
        if c is NotImplemented:
            return NotImplemented
        return c <= 0

    def __gt__(self, other):
        c = self._cmp(other)
        if c is NotImplemented:
            return NotImplemented
        return c > 0

    def __ge__(self, other):
        c = self._cmp(other)
        if c is NotImplemented:
            return NotImplemented
        return c >= 0

    # -- approximation ---------------------------------------------

    def limit_denominator(self, max_denominator=1000000):
        if max_denominator < 1:
            raise ValueError("max_denominator should be at least 1")
        if self._denominator <= max_denominator:
            return Fraction(self._numerator, self._denominator)
        p0, q0, p1, q1 = 0, 1, 1, 0
        n, d = self._numerator, self._denominator
        while True:
            a = n // d
            q2 = q0 + a * q1
            if q2 > max_denominator:
                break
            p0, q0, p1, q1 = p1, q1, p0 + a * p1, q2
            n, d = d, n - a * d
        k = (max_denominator - q0) // q1
        bound1 = Fraction(p0 + k * p1, q0 + k * q1)
        bound2 = Fraction(p1, q1)
        # Pick the bound that is closer to self. We compare distances
        # via signed-difference squared to avoid invoking abs() on
        # Fraction (the VM does not yet dispatch abs() into user
        # __abs__; we work around it here).
        d1 = bound1 - self
        d2 = bound2 - self
        if d1._numerator < 0:
            d1 = Fraction(-d1._numerator, d1._denominator)
        if d2._numerator < 0:
            d2 = Fraction(-d2._numerator, d2._denominator)
        if d2 <= d1:
            return bound2
        return bound1


_norm = _normalize


__all__ = ["Fraction"]
