"""RFC 0068 WS3 — long/float/complex/number/hash C-API fixture shims.

Python ports of the `Modules/_testlimitedcapi/{long,float,complex}.c`
and `Modules/_testcapi/{long,float,complex,numbers,hash}.c` fixture
wrappers, star-imported by the frozen `_testcapi` / `_testlimitedcapi`
shims.  Every exported name must be listed in `__all__`.

The C wrappers all run their METH_O argument through `NULLABLE(arg)`,
which maps Py_None to C NULL — so `None` here consistently plays the
role of NULL and triggers each C API's NULL behaviour (usually
`PyErr_BadInternalCall`, i.e. SystemError).
"""

import errno as _errno
import math as _math
import struct as _struct
import sys as _sys
import warnings as _warnings

import _weave_getargs as _ga

__all__ = [
    # Modules/_testcapi/long.c
    "call_long_compact_api",
    "pylong_fromunicodeobject",
    "pylong_asnativebytes",
    "pylong_fromnativebytes",
    "pylong_aspid",
    # Modules/_testlimitedcapi/long.c
    "pylong_check",
    "pylong_checkexact",
    "pylong_fromdouble",
    "pylong_fromstring",
    "pylong_fromvoidptr",
    "PyLong_AsInt",
    "pylong_aslong",
    "pylong_aslongandoverflow",
    "pylong_asunsignedlong",
    "pylong_asunsignedlongmask",
    "pylong_aslonglong",
    "pylong_aslonglongandoverflow",
    "pylong_asunsignedlonglong",
    "pylong_asunsignedlonglongmask",
    "pylong_as_ssize_t",
    "pylong_as_size_t",
    "pylong_asdouble",
    "pylong_asvoidptr",
    # Modules/_testcapi/immortal.c (also exercised by test_long's
    # test_bug_143050; the misc shim may override with its own copy —
    # it is star-imported after this module, so its version wins).
    "test_immortal_small_ints",
    # Modules/_testlimitedcapi/float.c
    "float_check",
    "float_checkexact",
    "float_fromstring",
    "float_fromdouble",
    "float_asdouble",
    "float_getinfo",
    "float_getmax",
    "float_getmin",
    # Modules/_testcapi/float.c
    "float_pack",
    "float_unpack",
    # Modules/_testlimitedcapi/complex.c
    "complex_check",
    "complex_checkexact",
    "complex_fromdoubles",
    "complex_realasdouble",
    "complex_imagasdouble",
    # Modules/_testcapi/complex.c
    "complex_fromccomplex",
    "complex_asccomplex",
    "_py_c_sum",
    "_py_c_diff",
    "_py_c_neg",
    "_py_c_prod",
    "_py_c_quot",
    "_py_c_pow",
    "_py_c_abs",
    # Modules/_testcapi/numbers.c
    "number_check",
    "number_add",
    "number_subtract",
    "number_multiply",
    "number_matrixmultiply",
    "number_floordivide",
    "number_truedivide",
    "number_remainder",
    "number_divmod",
    "number_power",
    "number_negative",
    "number_positive",
    "number_absolute",
    "number_invert",
    "number_lshift",
    "number_rshift",
    "number_and",
    "number_xor",
    "number_or",
    "number_inplaceadd",
    "number_inplacesubtract",
    "number_inplacemultiply",
    "number_inplacematrixmultiply",
    "number_inplacefloordivide",
    "number_inplacetruedivide",
    "number_inplaceremainder",
    "number_inplacepower",
    "number_inplacelshift",
    "number_inplacershift",
    "number_inplaceand",
    "number_inplacexor",
    "number_inplaceor",
    "number_long",
    "number_float",
    "number_index",
    "number_tobase",
    "number_asssizet",
    # Modules/_testcapi/hash.c
    "hash_getfuncdef",
    "hash_pointer",
]


_ULL_MASK = (1 << 64) - 1
_SSIZE_MAX = (1 << 63) - 1
_SSIZE_MIN = -(1 << 63)
# A CPython "compact" int fits in a single 30-bit digit.
_COMPACT_BOUND = 1 << 30


def _null_err():
    # PyErr_BadInternalCall()
    raise SystemError("bad argument to internal function")


def _index(arg, null=True):
    """_PyNumber_Index with the wrappers' NULL(=None) → SystemError leg."""
    if arg is None and null:
        _null_err()
    return _ga._py_index(arg)


def _exact_int(arg):
    """PyLong_Check gate + exact value (a subclass's own __index__ is
    never consulted for int instances)."""
    if arg is None:
        _null_err()
    if not isinstance(arg, int):
        raise TypeError(
            "an integer is required (got type %s)" % type(arg).__name__
        )
    return arg + 0


def _as_signed(arg, bits, what):
    # PyLong_As{Int,Long,LongLong,Pid}: __index__ accepted, range-checked.
    v = _index(arg)
    if not -(1 << (bits - 1)) <= v <= (1 << (bits - 1)) - 1:
        raise OverflowError("Python int too large to convert to C " + what)
    return v


def _as_signed_overflow(arg, bits):
    # PyLong_As{Long,LongLong}AndOverflow.
    v = _index(arg)
    if v > (1 << (bits - 1)) - 1:
        return (-1, 1)
    if v < -(1 << (bits - 1)):
        return (-1, -1)
    return (v, 0)


def _as_unsigned_strict(arg, bits, what):
    # PyLong_As{UnsignedLong,UnsignedLongLong,Size_t}: exact int only
    # (no __index__), negative → OverflowError.
    v = _exact_int(arg)
    if v < 0:
        raise OverflowError(
            "can't convert negative value to unsigned " + what
        )
    if v > (1 << bits) - 1:
        raise OverflowError(
            "Python int too large to convert to C unsigned " + what
        )
    return v


def _as_unsigned_mask(arg, bits):
    # PyLong_AsUnsigned{Long,LongLong}Mask: __index__ accepted, wraps.
    if arg is None:
        _null_err()
    return _ga._py_index(arg) & ((1 << bits) - 1)


# ---------------------------------------------------------------------------
# Modules/_testcapi/long.c


def call_long_compact_api(arg):
    v = arg + 0
    is_compact = -_COMPACT_BOUND < v < _COMPACT_BOUND
    return (is_compact, v if is_compact else -1)


def pylong_fromunicodeobject(arg, base):
    # PyLong_FromUnicodeObject: non-ASCII decimal digits accepted,
    # embedded NULs rejected — exactly int(str, base).
    return int(arg, base)


def pylong_aspid(arg):
    # PyLong_AsPid; pid_t is 32-bit (SIZEOF_PID_T == 4).
    return _as_signed(arg, 32, "pid_t")


def _fits_in_n_bits(v, nbits):
    return (v >> (nbits - 1)) in (0, -1)


def _resolve_endian(flags):
    # Py_ASNATIVEBYTES: -1/NATIVE_ENDIAN(2-bit) → platform little-endian.
    if flags == -1 or (flags & 2):
        return True
    return bool(flags & 1)


def pylong_asnativebytes(v, buffer, n, flags):
    """Port of PyLong_AsNativeBytes() (Objects/longobject.c) plus the
    `pylong_asnativebytes` wrapper's own buffer checks."""
    if not isinstance(buffer, (bytearray, memoryview)):
        raise TypeError("buffer must be writable")
    if isinstance(buffer, memoryview) and buffer.readonly:
        raise TypeError("buffer must be writable")
    n = _ga._py_index(n)
    flags = _ga._py_index(flags)
    if len(buffer) < n:
        raise ValueError("buffer must be at least 'n' bytes")
    if n < 0:
        _null_err()

    le = _resolve_endian(flags)

    if isinstance(v, int):
        val = v + 0
    elif flags != -1 and (flags & 16):  # Py_ASNATIVEBYTES_ALLOW_INDEX
        val = _ga._py_index(v)
    else:
        raise TypeError("expect int, got %s" % type(v).__name__)

    if flags != -1 and (flags & 8) and val < 0:  # REJECT_NEGATIVE
        raise ValueError("Cannot convert negative int")

    unsigned_ok = flags == -1 or (flags & 4)  # UNSIGNED_BUFFER

    if -_COMPACT_BOUND < val < _COMPACT_BOUND:
        # Compact path: the value is copied out of a native Py_ssize_t.
        res = 8
        cv = (val & _ULL_MASK).to_bytes(8, "little")
        if n <= 0:
            pass
        elif n <= 8:
            buffer[0:n] = cv[:n] if le else cv[:n][::-1]
            if _fits_in_n_bits(val, n * 8):
                res = n
            elif val > 0 and _fits_in_n_bits(val, n * 8 + 1):
                res = n if unsigned_ok else n + 1
        else:
            fill = b"\xff" if val < 0 else b"\x00"
            if le:
                buffer[0:n] = cv + fill * (n - 8)
            else:
                buffer[0:n] = fill * (n - 8) + cv[::-1]
        return res

    # Multi-digit path: _PyLong_AsByteArray fills all n bytes (low bits,
    # two's complement) even when the value does not fit.
    if n > 0:
        mask = (1 << (8 * n)) - 1
        buffer[0:n] = (val & mask).to_bytes(n, "little" if le else "big")
    nb = abs(val).bit_length()  # _PyLong_NumBits (magnitude only)
    res = nb // 8 + 1
    if n > 0 and res == n + 1 and nb % 8 == 0:
        b = bytes(buffer[0:n])
        if val < 0:
            # -2**(8n-1) exactly: sign bit needs no extra byte.
            pattern = (
                b"\x00" * (n - 1) + b"\x80" if le
                else b"\x80" + b"\x00" * (n - 1)
            )
            if b == pattern:
                res = n
        else:
            if b[n - 1 if le else 0] & 0x80:
                res = n if unsigned_ok else n + 1
    return res


def pylong_fromnativebytes(buffer, n, flags, signed_):
    # PyLong_FromNativeBytes / PyLong_FromUnsignedNativeBytes.
    data = bytes(buffer)
    n = _ga._py_index(n)
    flags = _ga._py_index(flags)
    if len(data) < n:
        raise ValueError("buffer must be at least 'n' bytes")
    le = _resolve_endian(flags)
    if signed_:
        signed = flags == -1 or not (flags & 4)
    else:
        signed = False
    return int.from_bytes(data[:n], "little" if le else "big", signed=signed)


class _CFuncNum:
    """Non-descriptor callable: test_misc splices `test_*` fixtures into
    a TestCase namespace (`locals().update(get_test_funcs(...))`), where
    a plain function would wrongly bind `self` like a method. A C
    builtin doesn't; neither does this wrapper."""

    def __init__(self, fn):
        self._fn = fn
        self.__name__ = getattr(fn, "__name__", "cfunc")

    def __call__(self, *args, **kwargs):
        return self._fn(*args, **kwargs)


def _test_immortal_small_ints_impl():
    # C leg checks the interned small-int singletons weren't corrupted
    # (gh-143050 mutated them via _pylong.int_from_string).  Verify the
    # cached values still round-trip.
    for i in range(-5, 257):
        if int(str(i)) != i or i + 0 != i:
            raise AssertionError(
                "test_immortal_small_ints: small int %d corrupted" % i
            )
    return None


test_immortal_small_ints = _CFuncNum(_test_immortal_small_ints_impl)


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/long.c


def pylong_check(obj):
    return 1 if isinstance(obj, int) else 0


def pylong_checkexact(obj):
    return 1 if type(obj) is int else 0


def pylong_fromdouble(arg):
    d = _ga._as_double(arg)
    # int() raises the same OverflowError (inf) / ValueError (nan)
    # as PyLong_FromDouble.
    return int(d)


def pylong_fromstring(data, base):
    # PyLong_FromString sees a NUL-terminated char*; `end` lands after
    # any trailing whitespace, i.e. at the terminating NUL.
    if isinstance(data, str):
        data = data.encode("utf-8")
    s = bytes(data).split(b"\x00", 1)[0]
    return (int(s, base), len(s))


_voidptr_registry = {}


def pylong_fromvoidptr(arg):
    # PyLong_FromVoidPtr((void *)arg): the object's own address.  Keep
    # a registry so pylong_asvoidptr can hand the object back.
    if arg is None:
        return 0
    addr = id(arg) & _ULL_MASK
    _voidptr_registry[addr] = arg
    return addr


def pylong_asvoidptr(arg):
    # PyLong_AsVoidPtr: negative ints via AsLongLong, others via
    # AsUnsignedLongLong (both exact-int only).
    v = _exact_int(arg)
    if v < 0:
        if v < -(1 << 63):
            raise OverflowError(
                "Python int too large to convert to C long long"
            )
    elif v > _ULL_MASK:
        raise OverflowError(
            "Python int too large to convert to C unsigned long long"
        )
    addr = v & _ULL_MASK
    if addr == 0:
        return None
    try:
        return _voidptr_registry[addr]
    except KeyError:
        raise SystemError("pylong_asvoidptr: pointer does not designate "
                          "a live fixture object") from None


def PyLong_AsInt(arg):
    return _as_signed(arg, 32, "int")


def pylong_aslong(arg):
    return _as_signed(arg, 64, "long")


def pylong_aslongandoverflow(arg):
    return _as_signed_overflow(arg, 64)


def pylong_asunsignedlong(arg):
    return _as_unsigned_strict(arg, 64, "long")


def pylong_asunsignedlongmask(arg):
    return _as_unsigned_mask(arg, 64)


def pylong_aslonglong(arg):
    return _as_signed(arg, 64, "long long")


def pylong_aslonglongandoverflow(arg):
    return _as_signed_overflow(arg, 64)


def pylong_asunsignedlonglong(arg):
    return _as_unsigned_strict(arg, 64, "long long")


def pylong_asunsignedlonglongmask(arg):
    return _as_unsigned_mask(arg, 64)


def pylong_as_ssize_t(arg):
    # PyLong_AsSsize_t: exact int only, signed range.
    v = _exact_int(arg)
    if not _SSIZE_MIN <= v <= _SSIZE_MAX:
        raise OverflowError("Python int too large to convert to C ssize_t")
    return v


def pylong_as_size_t(arg):
    return _as_unsigned_strict(arg, 64, "size_t")


def pylong_asdouble(arg):
    # PyLong_AsDouble: exact int only; float() raises the matching
    # OverflowError for huge magnitudes.
    return float(_exact_int(arg))


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/float.c + Modules/_testcapi/float.c


def float_check(obj):
    return 1 if isinstance(obj, float) else 0


def float_checkexact(obj):
    return 1 if type(obj) is float else 0


def float_fromstring(obj):
    # PyFloat_FromString: str / bytes / bytearray / C-contiguous buffer.
    if isinstance(obj, (str, bytes, bytearray)):
        return float(obj)
    if isinstance(obj, memoryview):
        if not obj.c_contiguous:
            raise TypeError(
                "float() argument must be a string or a number, "
                "not 'memoryview'"
            )
        return float(bytes(obj))
    raise TypeError(
        "float() argument must be a string or a number, not '%s'"
        % type(obj).__name__
    )


def float_fromdouble(arg):
    return _ga._as_double(arg)


def float_asdouble(obj):
    # PyFloat_AsDouble(NULL) → PyErr_BadArgument() (TypeError).
    if obj is None:
        raise TypeError("bad argument type for built-in operation")
    return _ga._as_double(obj)


def float_getinfo():
    return _sys.float_info


def float_getmax():
    return _sys.float_info.max


def float_getmin():
    return _sys.float_info.min


_PACK_FMTS = {2: "e", 4: "f", 8: "d"}


def float_pack(size, d, le):
    # PyFloat_Pack2/4/8; struct shares the rounding and the
    # OverflowError-on-out-of-range behaviour.
    d = _ga._as_double(d)
    fmt = _PACK_FMTS.get(size)
    if fmt is None:
        raise ValueError("size must 2, 4 or 8")
    return _struct.pack(("<" if le else ">") + fmt, d)


def float_unpack(data, le):
    data = bytes(data)
    fmt = _PACK_FMTS.get(len(data))
    if fmt is None:
        raise ValueError("data length must 2, 4 or 8 bytes")
    return _struct.unpack(("<" if le else ">") + fmt, data)[0]


# ---------------------------------------------------------------------------
# Modules/_testlimitedcapi/complex.c + Modules/_testcapi/complex.c


def complex_check(obj):
    return 1 if isinstance(obj, complex) else 0


def complex_checkexact(obj):
    return 1 if type(obj) is complex else 0


def complex_fromdoubles(real, imag):
    return complex(_ga._as_double(real), _ga._as_double(imag))


def complex_realasdouble(obj):
    # PyComplex_RealAsDouble: complex value (subclasses included), else
    # __complex__ (strict-subclass DeprecationWarning), else __float__.
    return _ga._as_complex(obj).real


def complex_imagasdouble(obj):
    return _ga._as_complex(obj).imag


def complex_fromccomplex(obj):
    # PyArg_Parse "D" + PyComplex_FromCComplex round trip.
    return _ga._as_complex(obj)


def complex_asccomplex(obj):
    # PyComplex_AsCComplex + PyComplex_FromCComplex round trip.
    return _ga._as_complex(obj)


def _py_c_sum(a, b):
    a = _ga._as_complex(a)
    b = _ga._as_complex(b)
    return (complex(a.real + b.real, a.imag + b.imag), 0)


def _py_c_diff(a, b):
    a = _ga._as_complex(a)
    b = _ga._as_complex(b)
    return (complex(a.real - b.real, a.imag - b.imag), 0)


def _py_c_neg(num):
    a = _ga._as_complex(num)
    return complex(-a.real, -a.imag)


def _py_c_prod(a, b):
    a = _ga._as_complex(a)
    b = _ga._as_complex(b)
    return (
        complex(
            a.real * b.real - a.imag * b.imag,
            a.real * b.imag + a.imag * b.real,
        ),
        0,
    )


def _py_c_quot(a, b):
    # Port of _Py_c_quot (Objects/complexobject.c, Smith's algorithm).
    a = _ga._as_complex(a)
    b = _ga._as_complex(b)
    abs_breal = -b.real if b.real < 0 else b.real
    abs_bimag = -b.imag if b.imag < 0 else b.imag
    if abs_breal >= abs_bimag:
        if abs_breal == 0.0:
            return (complex(0.0, 0.0), _errno.EDOM)
        ratio = b.imag / b.real
        denom = b.real + b.imag * ratio
        return (
            complex(
                (a.real + a.imag * ratio) / denom,
                (a.imag - a.real * ratio) / denom,
            ),
            0,
        )
    if abs_bimag >= abs_breal:
        ratio = b.real / b.imag
        denom = b.real * ratio + b.imag
        return (
            complex(
                (a.real * ratio + a.imag) / denom,
                (a.imag * ratio - a.real) / denom,
            ),
            0,
        )
    # At least one of b.real or b.imag is a NaN.
    return (complex(_math.nan, _math.nan), 0)


def _pow_or_inf(x, y):
    # C pow(): overflow yields HUGE_VAL (+inf for x > 0) with ERANGE;
    # the caller re-derives errno from the final result.
    try:
        return _math.pow(x, y)
    except OverflowError:
        return _math.inf


def _py_c_pow(a, b):
    # Port of _Py_c_pow (Objects/complexobject.c) with the wrapper's
    # errno reporting (_Py_ADJUST_ERANGE2).
    a = _ga._as_complex(a)
    b = _ga._as_complex(b)
    if b.real == 0.0 and b.imag == 0.0:
        return (complex(1.0, 0.0), 0)
    if a.real == 0.0 and a.imag == 0.0:
        err = _errno.EDOM if (b.imag != 0.0 or b.real < 0.0) else 0
        return (complex(0.0, 0.0), err)
    vabs = _math.hypot(a.real, a.imag)
    length = _pow_or_inf(vabs, b.real)
    at = _math.atan2(a.imag, a.real)
    phase = at * b.real
    if b.imag != 0.0:
        try:
            d = _math.exp(at * b.imag)
        except OverflowError:
            d = _math.inf
        if d == 0.0:
            length = _math.nan if length == 0.0 else _math.inf
        else:
            length = length / d
        phase += b.imag * _math.log(vabs)
    real = length * _math.cos(phase)
    imag = length * _math.sin(phase)
    err = _errno.ERANGE if (_math.isinf(real) or _math.isinf(imag)) else 0
    return (complex(real, imag), err)


def _py_c_abs(obj):
    # Port of _Py_c_abs (Objects/complexobject.c).
    z = _ga._as_complex(obj)
    if not _math.isfinite(z.real) or not _math.isfinite(z.imag):
        if _math.isinf(z.real):
            return (abs(z.real), 0)
        if _math.isinf(z.imag):
            return (abs(z.imag), 0)
        return (_math.nan, 0)
    try:
        result = _math.hypot(z.real, z.imag)
    except OverflowError:
        return (_math.inf, _errno.ERANGE)
    # Finite inputs overflowing to inf set ERANGE in C.
    return (result, _errno.ERANGE if _math.isinf(result) else 0)


# ---------------------------------------------------------------------------
# Modules/_testcapi/numbers.c


def number_check(obj):
    # PyNumber_Check: nb_index/nb_int/nb_float slot or complex.
    if obj is None:
        return False
    t = type(obj)
    return bool(
        hasattr(t, "__index__")
        or hasattr(t, "__int__")
        or hasattr(t, "__float__")
        or isinstance(obj, complex)
    )


def _unaryfunc(op):
    def fixture(obj):
        if obj is None:
            _null_err()
        return op(obj)

    return fixture


def _binaryfunc(op):
    def fixture(o1, o2):
        return op(o1, o2)

    return fixture


import operator as _operator  # noqa: E402

number_negative = _unaryfunc(_operator.neg)
number_positive = _unaryfunc(_operator.pos)
number_absolute = _unaryfunc(_operator.abs)
number_invert = _unaryfunc(_operator.invert)

number_add = _binaryfunc(_operator.add)
number_subtract = _binaryfunc(_operator.sub)
number_multiply = _binaryfunc(_operator.mul)
number_matrixmultiply = _binaryfunc(_operator.matmul)
number_floordivide = _binaryfunc(_operator.floordiv)
number_truedivide = _binaryfunc(_operator.truediv)
number_remainder = _binaryfunc(_operator.mod)
number_divmod = _binaryfunc(divmod)
number_lshift = _binaryfunc(_operator.lshift)
number_rshift = _binaryfunc(_operator.rshift)
number_and = _binaryfunc(_operator.and_)
number_xor = _binaryfunc(_operator.xor)
number_or = _binaryfunc(_operator.or_)

number_inplaceadd = _binaryfunc(_operator.iadd)
number_inplacesubtract = _binaryfunc(_operator.isub)
number_inplacemultiply = _binaryfunc(_operator.imul)
number_inplacematrixmultiply = _binaryfunc(_operator.imatmul)
number_inplacefloordivide = _binaryfunc(_operator.ifloordiv)
number_inplacetruedivide = _binaryfunc(_operator.itruediv)
number_inplaceremainder = _binaryfunc(_operator.imod)
number_inplacelshift = _binaryfunc(_operator.ilshift)
number_inplacershift = _binaryfunc(_operator.irshift)
number_inplaceand = _binaryfunc(_operator.iand)
number_inplacexor = _binaryfunc(_operator.ixor)
number_inplaceor = _binaryfunc(_operator.ior)


def number_power(o1, o2, o3=None):
    # PyNumber_Power; the wrapper defaults the third argument to
    # Py_None, and pow(a, b, None) is exactly the binary form.
    return pow(o1, o2, o3)


def number_inplacepower(o1, o2, o3=None):
    # PyNumber_InPlacePower: the __ipow__ slot is binary (the third
    # argument is dropped); without it, fall back to ternary power.
    f = getattr(type(o1), "__ipow__", None)
    if f is not None:
        r = f(o1, o2)
        if r is not NotImplemented:
            return r
    return pow(o1, o2, o3)


def number_long(obj):
    # PyNumber_Long: nb_int, nb_index, then str/bytes/buffer parsing.
    if obj is None:
        _null_err()
    t = type(obj)
    f = getattr(t, "__int__", None)
    if f is not None:
        r = f(obj)
        if not isinstance(r, int):
            raise TypeError(
                "__int__ returned non-int (type %s)" % type(r).__name__
            )
        if type(r) is not int:
            _warnings.warn(
                "__int__ returned non-int (type %s).  "
                "The ability to return an instance of a strict subclass"
                " of int is deprecated, and may be removed in a future "
                "version of Python." % type(r).__name__,
                DeprecationWarning,
                stacklevel=2,
            )
        return r + 0
    if hasattr(t, "__index__"):
        return _ga._py_index(obj)
    if isinstance(obj, (bytes, bytearray, memoryview)):
        return int(bytes(obj))
    return int(obj)


def number_float(obj):
    # PyNumber_Float: nb_float/nb_index protocol, else string parsing.
    if obj is None:
        _null_err()
    t = type(obj)
    if (
        isinstance(obj, (float, int))
        or hasattr(t, "__float__")
        or hasattr(t, "__index__")
    ):
        return _ga._as_double(obj)
    return float(obj)


def number_index(obj):
    if obj is None:
        _null_err()
    return _ga._py_index(obj)


def number_tobase(n, base):
    if base not in (2, 8, 10, 16):
        raise SystemError(
            "PyNumber_ToBase: base must be 2, 8, 10 or 16"
        )
    if n is None:
        _null_err()
    v = _ga._py_index(n)
    if base == 2:
        return bin(v)
    if base == 8:
        return oct(v)
    if base == 10:
        return str(v)
    return hex(v)


def number_asssizet(obj, exc):
    # PyNumber_AsSsize_t: NULL exc clamps instead of raising.
    if obj is None:
        _null_err()
    v = _ga._py_index(obj)
    if v > _SSIZE_MAX or v < _SSIZE_MIN:
        if exc is None:
            return _SSIZE_MAX if v > 0 else _SSIZE_MIN
        raise exc(
            "cannot fit '%s' into an index-sized integer"
            % type(obj).__name__
        )
    return v


# ---------------------------------------------------------------------------
# Modules/_testcapi/hash.c


def hash_getfuncdef():
    # PyHash_GetFuncDef mirrored off sys.hash_info.
    import types

    info = _sys.hash_info
    return types.SimpleNamespace(
        name=info.algorithm,
        hash_bits=info.hash_bits,
        seed_bits=info.seed_bits,
    )


def hash_pointer(arg):
    # PyLong_AsVoidPtr, then Py_HashPointer: rotate the pointer right
    # by 4 bits; -1 is reserved for errors so it maps to -2.
    v = _exact_int(arg)
    if v < -(1 << 63) or v > _ULL_MASK:
        raise OverflowError("Python int too large to convert to C pointer")
    x = v & _ULL_MASK
    x = ((x >> 4) | ((x & 15) << 60)) & _ULL_MASK
    if x > _SSIZE_MAX:
        x -= 1 << 64
    if x == -1:
        x = -2
    return x
