"""Public ``decimal`` module (RFC 0019).

Pure-Python implementation of arbitrary-precision base-10 arithmetic
sitting directly on top of WeavePy's bignum ``int`` type. It mirrors
the most-used pieces of CPython's :class:`decimal.Decimal` API:

* Construction from ``int``, ``str``, ``float``, ``Decimal``, and
  ``(sign, digits_tuple, exponent)`` triples (matches CPython 3.x).
* ``__add__`` / ``__sub__`` / ``__mul__`` / ``__truediv__`` /
  ``__neg__`` / ``__abs__`` / ``__pow__`` (integer exponents).
* Comparison operators, hashing, ``bool``, ``int``, ``float``.
* Quantize-style ``quantize(other, rounding=...)`` plus the standard
  rounding mode constants and a thread-local ``getcontext()``.
* ``Decimal('3.14').as_tuple()`` and ``as_integer_ratio()``.

It does *not* yet implement the full IEEE 754-2008 decimal context
(traps, signal flags, NaN/Infinity flavours), or the exhaustive
formatting / parsing rules CPython's ``_decimal`` module exposes.
Those are flagged as RFC follow-ups.
"""

import math
import re

ROUND_HALF_UP = "ROUND_HALF_UP"
ROUND_HALF_EVEN = "ROUND_HALF_EVEN"
ROUND_HALF_DOWN = "ROUND_HALF_DOWN"
ROUND_DOWN = "ROUND_DOWN"
ROUND_UP = "ROUND_UP"
ROUND_FLOOR = "ROUND_FLOOR"
ROUND_CEILING = "ROUND_CEILING"
ROUND_05UP = "ROUND_05UP"

_PARSE = re.compile(
    r"""\A
    (?P<sign>[-+])?
    (?:
        (?P<int>\d+)(?:\.(?P<frac>\d*))?
        |
        \.(?P<frac2>\d+)
    )
    (?:[eE](?P<exp>[-+]?\d+))?
    \Z""",
    re.VERBOSE,
)


class DecimalException(ArithmeticError):
    pass


class InvalidOperation(DecimalException):
    pass


class DivisionByZero(DecimalException, ZeroDivisionError):
    pass


class Inexact(DecimalException):
    pass


class Rounded(DecimalException):
    pass


class Subnormal(DecimalException):
    pass


class Overflow(DecimalException, OverflowError):
    pass


class Underflow(DecimalException):
    pass


class Clamped(DecimalException):
    pass


class FloatOperation(DecimalException, TypeError):
    pass


class _Context:
    def __init__(self, prec=28, rounding=ROUND_HALF_EVEN):
        self.prec = prec
        self.rounding = rounding

    def copy(self):
        return _Context(self.prec, self.rounding)


_default_context = _Context()


def getcontext():
    return _default_context


def setcontext(ctx):
    global _default_context
    _default_context = ctx


def localcontext(ctx=None):
    return _LocalContext(ctx or _default_context.copy())


class _LocalContext:
    def __init__(self, ctx):
        self.ctx = ctx
        self._prev = None

    def __enter__(self):
        global _default_context
        self._prev = _default_context
        _default_context = self.ctx
        return self.ctx

    def __exit__(self, *exc):
        global _default_context
        _default_context = self._prev
        return False


class Decimal:
    """Arbitrary-precision decimal number."""

    __slots__ = ("_sign", "_int", "_exp")

    def __new__(cls, value="0", context=None):
        self = object.__new__(cls)
        if isinstance(value, Decimal):
            self._sign = value._sign
            self._int = value._int
            self._exp = value._exp
            return self
        if isinstance(value, int):
            self._sign = 0 if value >= 0 else 1
            self._int = abs(value)
            self._exp = 0
            return self
        if isinstance(value, float):
            return cls.from_float(value)
        if isinstance(value, tuple):
            if len(value) != 3:
                raise InvalidOperation("Invalid Decimal tuple: %r" % (value,))
            sign, digits, exp = value
            self._sign = sign
            self._int = 0
            for d in digits:
                self._int = self._int * 10 + int(d)
            self._exp = exp
            return self
        if isinstance(value, str):
            text = value.strip().replace("_", "")
            if not text:
                raise InvalidOperation("Invalid Decimal string: %r" % value)
            m = _PARSE.match(text)
            if not m:
                lower = text.lower().lstrip("+-")
                if lower in ("inf", "infinity"):
                    raise InvalidOperation(
                        "Decimal infinity not supported in this implementation")
                if lower == "nan":
                    raise InvalidOperation(
                        "Decimal NaN not supported in this implementation")
                raise InvalidOperation("Invalid Decimal string: %r" % value)
            sign = 1 if m.group("sign") == "-" else 0
            int_part = m.group("int") or ""
            frac_part = m.group("frac") or m.group("frac2") or ""
            digits = (int_part + frac_part).lstrip("0") or "0"
            exp = int(m.group("exp") or "0") - len(frac_part)
            self._sign = sign
            self._int = int(digits)
            self._exp = exp
            return self
        raise TypeError("Cannot convert %r to Decimal" % value)

    @classmethod
    def from_float(cls, f):
        if math.isnan(f):
            raise InvalidOperation("cannot convert NaN")
        if math.isinf(f):
            raise InvalidOperation("cannot convert infinity")
        n, d = f.as_integer_ratio()
        # Express n/d as exact decimal: n/d = n/(2^k * 5^j) — but we
        # only have d as a power of 2. Multiply numerator by 5**k.
        k = 0
        while d % 2 == 0:
            d //= 2
            k += 1
        n *= 5 ** k
        exp = -k
        sign = 1 if n < 0 else 0
        n = abs(n)
        return Decimal((sign, _digits(n), exp))

    # -- accessors -------------------------------------------------

    def as_tuple(self):
        return (self._sign, _digits(self._int), self._exp)

    def as_integer_ratio(self):
        if self._exp >= 0:
            num = self._int * (10 ** self._exp)
            den = 1
        else:
            num = self._int
            den = 10 ** (-self._exp)
            from math import gcd
            g = gcd(num, den)
            if g > 1:
                num //= g
                den //= g
        if self._sign:
            num = -num
        return (num, den)

    def is_zero(self):
        return self._int == 0

    # -- conversions ----------------------------------------------

    def __repr__(self):
        return "Decimal('%s')" % self

    def __str__(self):
        sign = "-" if self._sign else ""
        if self._exp >= 0:
            digits = str(self._int)
            return sign + digits + "0" * self._exp if self._exp else sign + digits
        digits = str(self._int)
        n = -self._exp
        if n >= len(digits):
            digits = "0" * (n - len(digits) + 1) + digits
        return sign + digits[:-n] + "." + digits[-n:]

    def __int__(self):
        if self._exp >= 0:
            v = self._int * (10 ** self._exp)
        else:
            v = self._int // (10 ** -self._exp)
        return -v if self._sign else v

    def __float__(self):
        return float(str(self))

    def __bool__(self):
        return self._int != 0

    def __hash__(self):
        if self._exp >= 0:
            return hash(int(self))
        return hash(self.as_integer_ratio())

    # -- arithmetic ------------------------------------------------

    def _to_signed_value(self):
        """Return ``(value, exp)`` such that ``value * 10**exp == self``."""
        v = -self._int if self._sign else self._int
        return v, self._exp

    @staticmethod
    def _align(a, b):
        """Bring two decimals to the same exponent."""
        ia, ea = a._to_signed_value()
        ib, eb = b._to_signed_value()
        if ea < eb:
            ib *= 10 ** (eb - ea)
            return ia, ib, ea
        if eb < ea:
            ia *= 10 ** (ea - eb)
            return ia, ib, eb
        return ia, ib, ea

    @staticmethod
    def _from_signed(value, exp):
        if value == 0:
            return Decimal((0, (0,), exp))
        sign = 1 if value < 0 else 0
        return Decimal((sign, _digits(abs(value)), exp))

    def __add__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        ia, ib, e = Decimal._align(self, other)
        return Decimal._from_signed(ia + ib, e)

    __radd__ = __add__

    def __sub__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        ia, ib, e = Decimal._align(self, other)
        return Decimal._from_signed(ia - ib, e)

    def __rsub__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        return other.__sub__(self)

    def __mul__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        ia, ea = self._to_signed_value()
        ib, eb = other._to_signed_value()
        return Decimal._from_signed(ia * ib, ea + eb)

    __rmul__ = __mul__

    def __truediv__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        if other.is_zero():
            raise DivisionByZero("Decimal division by zero")
        prec = getcontext().prec
        a = -self._int if self._sign else self._int
        b = -other._int if other._sign else other._int
        ea, eb = self._exp, other._exp
        # Scale a by 10^prec to retain precision.
        scale = prec + 1
        q, _ = divmod(a * (10 ** scale), b)
        return Decimal._from_signed(q, ea - eb - scale)._round(prec)

    def __rtruediv__(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        return other.__truediv__(self)

    def __neg__(self):
        if self._int == 0:
            return self
        return Decimal((1 - self._sign, _digits(self._int), self._exp))

    def __pos__(self):
        return self

    def __abs__(self):
        if self._sign == 0 or self._int == 0:
            return self
        return Decimal((0, _digits(self._int), self._exp))

    def __pow__(self, other):
        if not isinstance(other, int):
            return NotImplemented
        if other < 0:
            return Decimal(1) / (self ** -other)
        result = Decimal(1)
        base = self
        n = other
        while n > 0:
            if n & 1:
                result = result * base
            base = base * base
            n >>= 1
        return result

    # -- comparisons ----------------------------------------------

    def _cmp(self, other):
        other = _coerce(other)
        if other is NotImplemented:
            return NotImplemented
        ia, ib, _ = Decimal._align(self, other)
        return (ia > ib) - (ia < ib)

    def __eq__(self, other):
        c = self._cmp(other)
        if c is NotImplemented:
            return NotImplemented
        return c == 0

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

    # -- precision tools ------------------------------------------

    def _round(self, prec, rounding=None):
        if self._int == 0:
            return self
        rounding = rounding or getcontext().rounding
        digits = _digits(self._int)
        excess = len(digits) - prec
        if excess <= 0:
            return self
        kept = self._int // (10 ** excess)
        rem = self._int % (10 ** excess)
        threshold = 10 ** excess
        if rounding == ROUND_DOWN:
            new = kept
        elif rounding == ROUND_UP:
            new = kept + (1 if rem else 0)
        elif rounding == ROUND_HALF_UP:
            new = kept + (1 if rem * 2 >= threshold else 0)
        elif rounding == ROUND_HALF_DOWN:
            new = kept + (1 if rem * 2 > threshold else 0)
        elif rounding == ROUND_HALF_EVEN:
            doubled = rem * 2
            if doubled > threshold or (doubled == threshold and kept % 2):
                new = kept + 1
            else:
                new = kept
        elif rounding == ROUND_FLOOR:
            new = kept + (1 if (self._sign and rem) else 0)
        elif rounding == ROUND_CEILING:
            new = kept + (1 if (not self._sign and rem) else 0)
        else:
            new = kept
        sign = self._sign
        return Decimal((sign, _digits(new), self._exp + excess))

    def quantize(self, exp, rounding=None):
        if isinstance(exp, Decimal):
            target = exp._exp
        else:
            target = int(exp)
        if target < self._exp:
            scale = self._exp - target
            new_int = self._int * (10 ** scale)
            return Decimal((self._sign, _digits(new_int), target))
        if target > self._exp:
            shift = target - self._exp
            scaled = self._int
            divisor = 10 ** shift
            quot, rem = divmod(scaled, divisor)
            rounding = rounding or getcontext().rounding
            doubled = rem * 2
            if rounding == ROUND_HALF_EVEN:
                if doubled > divisor or (doubled == divisor and quot % 2):
                    quot += 1
            elif rounding == ROUND_HALF_UP:
                if doubled >= divisor:
                    quot += 1
            elif rounding == ROUND_HALF_DOWN:
                if doubled > divisor:
                    quot += 1
            elif rounding == ROUND_UP:
                if rem != 0:
                    quot += 1
            elif rounding == ROUND_FLOOR:
                if self._sign and rem:
                    quot += 1
            elif rounding == ROUND_CEILING:
                if not self._sign and rem:
                    quot += 1
            return Decimal((self._sign, _digits(quot), target))
        return Decimal(self)

    def normalize(self):
        if self._int == 0:
            return Decimal((self._sign, (0,), 0))
        n = self._int
        e = self._exp
        while n % 10 == 0:
            n //= 10
            e += 1
        return Decimal((self._sign, _digits(n), e))


def _coerce(other):
    if isinstance(other, Decimal):
        return other
    if isinstance(other, int):
        return Decimal(other)
    return NotImplemented


def _digits(n):
    if n == 0:
        return (0,)
    out = []
    while n:
        out.append(n % 10)
        n //= 10
    out.reverse()
    return tuple(out)


__all__ = ["Decimal", "DecimalException", "InvalidOperation",
           "DivisionByZero", "Inexact", "Rounded", "Subnormal",
           "Overflow", "Underflow", "Clamped", "FloatOperation",
           "ROUND_HALF_UP", "ROUND_HALF_EVEN", "ROUND_HALF_DOWN",
           "ROUND_DOWN", "ROUND_UP", "ROUND_FLOOR", "ROUND_CEILING",
           "ROUND_05UP",
           "getcontext", "setcontext", "localcontext"]
