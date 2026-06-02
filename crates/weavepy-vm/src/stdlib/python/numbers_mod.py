"""Abstract base classes for numbers — WeavePy port of CPython's
``numbers``.

Defines the numeric tower: Number → Complex → Real → Rational →
Integral. The classes are virtual ABCs; built-in numeric types
register themselves at import time so ``isinstance(3, Number)``
holds.
"""

from abc import ABCMeta, abstractmethod


class Number(metaclass=ABCMeta):
    """The abstract base class of the numeric tower."""

    __slots__ = ()
    __hash__ = None


class Complex(Number):
    """Numbers with a real and imaginary part."""

    __slots__ = ()

    @abstractmethod
    def __complex__(self):
        ...

    def __bool__(self):
        return self != 0

    @property
    @abstractmethod
    def real(self):
        ...

    @property
    @abstractmethod
    def imag(self):
        ...

    @abstractmethod
    def __add__(self, other):
        ...

    @abstractmethod
    def __radd__(self, other):
        ...

    @abstractmethod
    def __neg__(self):
        ...

    @abstractmethod
    def __pos__(self):
        ...

    def __sub__(self, other):
        return self + -other

    def __rsub__(self, other):
        return -self + other

    @abstractmethod
    def __mul__(self, other):
        ...

    @abstractmethod
    def __rmul__(self, other):
        ...

    @abstractmethod
    def __truediv__(self, other):
        ...

    @abstractmethod
    def __rtruediv__(self, other):
        ...

    @abstractmethod
    def __pow__(self, exponent):
        ...

    @abstractmethod
    def __rpow__(self, base):
        ...

    @abstractmethod
    def __abs__(self):
        ...

    @abstractmethod
    def conjugate(self):
        ...

    @abstractmethod
    def __eq__(self, other):
        ...


Complex.register(complex)


class Real(Complex):
    """To Complex, Real adds the operations that work on real numbers."""

    __slots__ = ()

    @abstractmethod
    def __float__(self):
        ...

    @abstractmethod
    def __trunc__(self):
        ...

    @abstractmethod
    def __floor__(self):
        ...

    @abstractmethod
    def __ceil__(self):
        ...

    @abstractmethod
    def __round__(self, ndigits=None):
        ...

    def __divmod__(self, other):
        return (self // other, self % other)

    def __rdivmod__(self, other):
        return (other // self, other % self)

    @abstractmethod
    def __floordiv__(self, other):
        ...

    @abstractmethod
    def __rfloordiv__(self, other):
        ...

    @abstractmethod
    def __mod__(self, other):
        ...

    @abstractmethod
    def __rmod__(self, other):
        ...

    @abstractmethod
    def __lt__(self, other):
        ...

    @abstractmethod
    def __le__(self, other):
        ...

    def __complex__(self):
        return complex(float(self), 0)

    @property
    def real(self):
        return +self

    @property
    def imag(self):
        return 0

    def conjugate(self):
        return +self


Real.register(float)


class Rational(Real):
    """The rationals add the methods numerator and denominator."""

    __slots__ = ()

    @property
    @abstractmethod
    def numerator(self):
        ...

    @property
    @abstractmethod
    def denominator(self):
        ...

    def __float__(self):
        """float(self) = self.numerator / self.denominator

        It's important that this conversion use the integer's "true"
        division rather than casting one side to float before dividing
        so that ratios of huge integers convert without overflowing.
        The explicit ``int()`` coercions let a Rational whose
        numerator/denominator are themselves Integral (but not built-in
        ``int``) still convert — e.g. ``DummyIntegral`` in the
        numeric-tower tests, whose own ``__truediv__`` declines.
        """
        return int(self.numerator) / int(self.denominator)


class Integral(Rational):
    """Integral adds methods that work on integers."""

    __slots__ = ()

    @abstractmethod
    def __int__(self):
        ...

    def __index__(self):
        return int(self)

    @abstractmethod
    def __pow__(self, exponent, modulus=None):
        ...

    @abstractmethod
    def __lshift__(self, other):
        ...

    @abstractmethod
    def __rlshift__(self, other):
        ...

    @abstractmethod
    def __rshift__(self, other):
        ...

    @abstractmethod
    def __rrshift__(self, other):
        ...

    @abstractmethod
    def __and__(self, other):
        ...

    @abstractmethod
    def __rand__(self, other):
        ...

    @abstractmethod
    def __xor__(self, other):
        ...

    @abstractmethod
    def __rxor__(self, other):
        ...

    @abstractmethod
    def __or__(self, other):
        ...

    @abstractmethod
    def __ror__(self, other):
        ...

    @abstractmethod
    def __invert__(self):
        ...

    def __float__(self):
        return float(int(self))

    @property
    def numerator(self):
        return +self

    @property
    def denominator(self):
        return 1


Integral.register(int)
Integral.register(bool)


__all__ = ["Number", "Complex", "Real", "Rational", "Integral"]
