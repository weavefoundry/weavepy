"""Faithful pure-Python ``cmath`` over the native ``math`` core.

CPython ships ``cmath`` as a C module (``Modules/cmathmodule.c``); WeavePy
provides a Python implementation that computes the same principal-branch
values via real :mod:`math` primitives and complex arithmetic. The public
surface (constants + functions) matches CPython 3.13 (RFC 0037 WS8).
"""

import math as _math

pi = _math.pi
e = _math.e
tau = _math.tau
inf = _math.inf
nan = _math.nan
infj = complex(0.0, _math.inf)
nanj = complex(0.0, _math.nan)

__all__ = [
    "pi", "e", "tau", "inf", "nan", "infj", "nanj",
    "phase", "polar", "rect",
    "exp", "log", "log10", "sqrt",
    "acos", "asin", "atan", "cos", "sin", "tan",
    "acosh", "asinh", "atanh", "cosh", "sinh", "tanh",
    "isfinite", "isinf", "isnan", "isclose",
]


def _c(z):
    """Coerce ``z`` to ``complex`` (accepting ints/floats and objects with
    ``__complex__``/``__float__``/``__index__``), matching cmath's argument
    handling."""
    if isinstance(z, complex):
        return z
    return complex(z)


def phase(z):
    z = _c(z)
    return _math.atan2(z.imag, z.real)


def polar(z):
    z = _c(z)
    return (abs(z), _math.atan2(z.imag, z.real))


def rect(r, phi):
    r = float(r)
    phi = float(phi)
    # Mirror CPython's special handling so rect(r, 0) keeps the sign of an
    # infinite/zero r on the real axis with a clean zero imaginary part.
    if phi == 0.0:
        return complex(r, 0.0 * r)
    return complex(r * _math.cos(phi), r * _math.sin(phi))


def isfinite(z):
    z = _c(z)
    return _math.isfinite(z.real) and _math.isfinite(z.imag)


def isinf(z):
    z = _c(z)
    return _math.isinf(z.real) or _math.isinf(z.imag)


def isnan(z):
    z = _c(z)
    return _math.isnan(z.real) or _math.isnan(z.imag)


def isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0):
    a = _c(a)
    b = _c(b)
    if rel_tol < 0.0 or abs_tol < 0.0:
        raise ValueError("tolerances must be non-negative")
    if a == b:
        return True
    if isinf(a) or isinf(b):
        return False
    diff = abs(a - b)
    return (diff <= abs(rel_tol * b)) or (diff <= abs(rel_tol * a)) or (diff <= abs_tol)


def exp(z):
    z = _c(z)
    r = _math.exp(z.real)
    return complex(r * _math.cos(z.imag), r * _math.sin(z.imag))


def log(z, base=None):
    z = _c(z)
    if base is not None:
        return log(z) / log(base)
    return complex(_math.log(abs(z)), _math.atan2(z.imag, z.real))


def log10(z):
    return log(z) / _math.log(10.0)


def sqrt(z):
    z = _c(z)
    if z.imag == 0.0 and z.real >= 0.0:
        return complex(_math.sqrt(z.real), 0.0)
    r = abs(z)
    ang = _math.atan2(z.imag, z.real) / 2.0
    m = _math.sqrt(r)
    return complex(m * _math.cos(ang), m * _math.sin(ang))


def cos(z):
    z = _c(z)
    return complex(_math.cos(z.real) * _math.cosh(z.imag),
                   -_math.sin(z.real) * _math.sinh(z.imag))


def sin(z):
    z = _c(z)
    return complex(_math.sin(z.real) * _math.cosh(z.imag),
                   _math.cos(z.real) * _math.sinh(z.imag))


def tan(z):
    z = _c(z)
    return sin(z) / cos(z)


def cosh(z):
    z = _c(z)
    return complex(_math.cosh(z.real) * _math.cos(z.imag),
                   _math.sinh(z.real) * _math.sin(z.imag))


def sinh(z):
    z = _c(z)
    return complex(_math.sinh(z.real) * _math.cos(z.imag),
                   _math.cosh(z.real) * _math.sin(z.imag))


def tanh(z):
    z = _c(z)
    return sinh(z) / cosh(z)


def asin(z):
    z = _c(z)
    return -1j * log(1j * z + sqrt(1 - z * z))


def acos(z):
    z = _c(z)
    return -1j * log(z + 1j * sqrt(1 - z * z))


def atan(z):
    z = _c(z)
    return (1j / 2) * (log(1 - 1j * z) - log(1 + 1j * z))


def asinh(z):
    z = _c(z)
    return log(z + sqrt(z * z + 1))


def acosh(z):
    z = _c(z)
    return log(z + sqrt(z - 1) * sqrt(z + 1))


def atanh(z):
    z = _c(z)
    return (log(1 + z) - log(1 - z)) / 2
