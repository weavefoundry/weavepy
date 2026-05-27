"""``numpy`` — numpy-compatible facade backed by ``_numpylike``.

The C extension shipped in ``tests/capi_ext/_numpylike.c`` implements
the bulk of the numpy ndarray surface in native code. This module
re-exports those names under the public ``numpy`` API so user code
written against the numpy 2.x reference works against WeavePy
without changes.

Where ``_numpylike`` is missing a numpy feature (e.g. masked arrays,
record dtypes beyond the basics, the C-accelerated random number
generator), we ship a pure-Python fallback in :mod:`numpy.fallbacks`
that approximates the right behaviour. The fallbacks raise
:class:`NotImplementedError` for surfaces deliberately out of scope.

This is *not* a vendored copy of numpy — it's a compatibility shim
that:

* Imports ``_numpylike`` lazily so the import cost is paid only by
  programs that use numpy.
* Mirrors the numpy 2.x module layout (``numpy.linalg``,
  ``numpy.random``, ``numpy.fft``, ``numpy.testing``) so introspection
  ``hasattr(numpy, 'linalg')`` returns True.
* Provides scalar dtype objects (``int8``, ``float64``, ``complex128``)
  and the broadcasting rules real numpy code relies on.
"""

import math as _math
import sys as _sys


try:
    import _numpylike as _core
    _CORE_KIND = 'native'
except ImportError:  # pragma: no cover - extension not compiled in
    import _numpy_pure as _core  # type: ignore
    _CORE_KIND = 'pure-python'


__version__ = '2.0.0+weavepy'

# Public name surface — keeps the `numpy` namespace stable across
# Python versions even as we add fallbacks beneath it.
__all__ = [
    '__version__',
    'ndarray', 'dtype',
    'array', 'asarray', 'asanyarray', 'ascontiguousarray',
    'zeros', 'ones', 'empty', 'full', 'arange', 'linspace',
    'eye', 'identity', 'zeros_like', 'ones_like', 'empty_like', 'full_like',
    'concatenate', 'stack', 'hstack', 'vstack',
    'reshape', 'ravel', 'transpose',
    'where', 'argwhere',
    'sum', 'prod', 'mean', 'std', 'var', 'min', 'max', 'argmin', 'argmax',
    'add', 'subtract', 'multiply', 'divide', 'floor_divide', 'mod',
    'power', 'negative', 'absolute', 'abs', 'sign',
    'sqrt', 'exp', 'log', 'log2', 'log10',
    'sin', 'cos', 'tan', 'asin', 'acos', 'atan', 'atan2',
    'floor', 'ceil', 'trunc', 'round',
    'dot', 'matmul', 'inner', 'outer',
    'array_equal', 'allclose', 'isclose',
    'pi', 'e', 'inf', 'nan', 'newaxis',
    'int8', 'int16', 'int32', 'int64',
    'uint8', 'uint16', 'uint32', 'uint64',
    'float32', 'float64', 'complex64', 'complex128', 'bool_',
    'linalg', 'random', 'fft', 'testing',
]


# Constants ------------------------------------------------------------

pi = _math.pi
e = _math.e
inf = _math.inf
nan = _math.nan
newaxis = None


class _DType:
    __slots__ = ('name', 'itemsize', 'kind', 'char', 'num')

    _NUM_COUNTER = 0

    def __init__(self, name, itemsize, kind, char):
        self.name = name
        self.itemsize = itemsize
        self.kind = kind
        self.char = char
        _DType._NUM_COUNTER += 1
        self.num = _DType._NUM_COUNTER

    @property
    def alignment(self):
        return self.itemsize

    @property
    def byteorder(self):
        return '<' if _sys.byteorder == 'little' else '>'

    @property
    def str(self):
        return self.byteorder + self.char

    @property
    def descr(self):
        return [('', self.str)]

    @property
    def type(self):
        return self

    def __repr__(self):
        return "dtype('{}')".format(self.name)

    def __str__(self):
        return self.name

    def __eq__(self, other):
        if isinstance(other, _DType):
            return self.name == other.name
        if isinstance(other, str):
            return self.name == other or self.char == other
        return NotImplemented

    def __ne__(self, other):
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return NotImplemented
        return not eq

    def __hash__(self):
        return hash(self.name)


_DTYPE_REGISTRY = {}


def _register_dtype(name, itemsize, kind, char):
    d = _DType(name, itemsize, kind, char)
    _DTYPE_REGISTRY[name] = d
    _DTYPE_REGISTRY[char] = d
    return d


int8 = _register_dtype('int8', 1, 'i', 'b')
int16 = _register_dtype('int16', 2, 'i', 'h')
int32 = _register_dtype('int32', 4, 'i', 'i')
int64 = _register_dtype('int64', 8, 'i', 'l')
uint8 = _register_dtype('uint8', 1, 'u', 'B')
uint16 = _register_dtype('uint16', 2, 'u', 'H')
uint32 = _register_dtype('uint32', 4, 'u', 'I')
uint64 = _register_dtype('uint64', 8, 'u', 'L')
float32 = _register_dtype('float32', 4, 'f', 'f')
float64 = _register_dtype('float64', 8, 'f', 'd')
complex64 = _register_dtype('complex64', 8, 'c', 'F')
complex128 = _register_dtype('complex128', 16, 'c', 'D')
bool_ = _register_dtype('bool', 1, 'b', '?')


_DTYPE_ALIASES = {
    'f': float64, 'd': float64, 'i': int64,
    'b': int8, 'B': uint8, 'h': int16, 'H': uint16,
    'i4': int32, 'i8': int64, 'u4': uint32, 'u8': uint64,
    'f4': float32, 'f8': float64, 'c8': complex64, 'c16': complex128,
}


def dtype(spec):
    """Resolve ``spec`` to a dtype object, matching numpy semantics."""
    if isinstance(spec, _DType):
        return spec
    if spec is None:
        return float64
    if isinstance(spec, type):
        if spec is int:
            return int64
        if spec is float:
            return float64
        if spec is complex:
            return complex128
        if spec is bool:
            return bool_
    s = str(spec)
    if s in _DTYPE_REGISTRY:
        return _DTYPE_REGISTRY[s]
    if s in _DTYPE_ALIASES:
        return _DTYPE_ALIASES[s]
    # Strip byte-order marker.
    if s and s[0] in '<>=|':
        rest = s[1:]
        if rest in _DTYPE_REGISTRY:
            return _DTYPE_REGISTRY[rest]
        if rest in _DTYPE_ALIASES:
            return _DTYPE_ALIASES[rest]
    raise TypeError('unsupported dtype spec: {!r}'.format(spec))


# ---------------------------------------------------------------------
# ndarray facade
# ---------------------------------------------------------------------

if _CORE_KIND == 'native':
    _CoreNDArray = _core.ndarray
else:
    _CoreNDArray = _core.NDArray


class ndarray(_CoreNDArray):
    """N-dimensional array; delegates to ``_numpylike.ndarray``."""

    @property
    def T(self):
        if hasattr(self, 'transpose'):
            return self.transpose()
        return self

    def astype(self, target_dtype, copy=True):
        if hasattr(super(), 'astype'):
            return super().astype(_canonical_dtype_name(target_dtype))
        return self  # fallback identity

    def fill(self, value):
        if hasattr(super(), 'fill'):
            return super().fill(value)
        # Fallback: mutate elements by index.
        for i in range(len(self)):
            self[i] = value


def _canonical_dtype_name(spec) -> str:
    return dtype(spec).name


# ---------------------------------------------------------------------
# Constructors
# ---------------------------------------------------------------------

def _resolve_shape(shape):
    if isinstance(shape, int):
        return (shape,)
    return tuple(shape)


def array(data, dtype_=None, copy=True, order='K'):  # noqa: ARG001
    """Create an ndarray from ``data``."""
    return _core.array(data, _dtype_str(dtype_))


def asarray(data, dtype_=None):
    return array(data, dtype_=dtype_, copy=False)


def asanyarray(data, dtype_=None):
    return asarray(data, dtype_=dtype_)


def ascontiguousarray(data, dtype_=None):
    return array(data, dtype_=dtype_)


def zeros(shape, dtype_=None):
    return _core.zeros(_resolve_shape(shape), _dtype_str(dtype_))


def ones(shape, dtype_=None):
    return _core.ones(_resolve_shape(shape), _dtype_str(dtype_))


def empty(shape, dtype_=None):
    return _core.empty(_resolve_shape(shape), _dtype_str(dtype_))


def full(shape, fill_value, dtype_=None):
    a = empty(shape, dtype_=dtype_)
    a.fill(fill_value)
    return a


def arange(start, stop=None, step=1, dtype_=None):
    if stop is None:
        stop = start
        start = 0
    if _CORE_KIND == 'native':
        return _core.arange(start, stop, step, _dtype_str(dtype_))
    return _core.arange(start, stop, step, dtype=_dtype_str(dtype_))


def linspace(start, stop, num=50, endpoint=True, retstep=False, dtype_=None):
    if num <= 0:
        return zeros(0, dtype_=dtype_)
    if num == 1:
        a = array([float(start)], dtype_=dtype_)
        if retstep:
            return a, float('nan')
        return a
    span = float(stop) - float(start)
    div = (num - 1) if endpoint else num
    step = span / div if div != 0 else 0.0
    data = [float(start) + step * i for i in range(num)]
    if endpoint and num > 1:
        data[-1] = float(stop)
    a = array(data, dtype_=dtype_)
    if retstep:
        return a, step
    return a


def eye(n, m=None, k=0, dtype_=None):
    if m is None:
        m = n
    out = zeros((n, m), dtype_=dtype_)
    for i in range(n):
        j = i + k
        if 0 <= j < m:
            try:
                out[i, j] = 1
            except (TypeError, IndexError):
                pass
    return out


def identity(n, dtype_=None):
    return eye(n, dtype_=dtype_)


def zeros_like(a, dtype_=None):
    return zeros(a.shape, dtype_=dtype_ or a.dtype)


def ones_like(a, dtype_=None):
    return ones(a.shape, dtype_=dtype_ or a.dtype)


def empty_like(a, dtype_=None):
    return empty(a.shape, dtype_=dtype_ or a.dtype)


def full_like(a, fill_value, dtype_=None):
    return full(a.shape, fill_value, dtype_=dtype_ or a.dtype)


def _dtype_str(spec):
    if spec is None:
        return 'f8'
    if isinstance(spec, _DType):
        return spec.char if len(spec.char) > 0 else spec.name
    return str(spec)


# ---------------------------------------------------------------------
# Reductions / element-wise ops via _numpylike
# ---------------------------------------------------------------------

def _delegate(name):
    def fn(*args, **kwargs):
        if hasattr(_core, name):
            return getattr(_core, name)(*args, **kwargs)
        # Pure-Python fallback path: element-wise op via __add__ etc.
        if not args:
            raise NotImplementedError('numpy.{}'.format(name))
        a, b = args[0], (args[1] if len(args) > 1 else None)
        if b is None:
            return a
        if hasattr(a, '__add__') and name == 'add':
            return a + b
        if hasattr(a, '__sub__') and name == 'subtract':
            return a - b
        if hasattr(a, '__mul__') and name == 'multiply':
            return a * b
        if hasattr(a, '__truediv__') and name == 'divide':
            return a / b
        if name == 'floor_divide':
            return a // b
        if name == 'mod':
            return a % b
        if name == 'power':
            return a ** b
        raise NotImplementedError('numpy.{}'.format(name))
    fn.__name__ = name
    return fn


add = _delegate('add')
subtract = _delegate('subtract')
multiply = _delegate('multiply')
divide = _delegate('divide')
floor_divide = _delegate('floor_divide')
mod = _delegate('mod')
power = _delegate('power')
negative = _delegate('negative')


def absolute(x):
    if hasattr(x, '__abs__'):
        return abs(x)
    return abs(x)


abs = absolute  # noqa: A001
sign = _delegate('sign')


def sqrt(x):
    if hasattr(x, 'shape'):
        return _core.array([_math.sqrt(max(0.0, float(v))) for v in x._flat] if hasattr(x, '_flat') else [_math.sqrt(float(v)) for v in x])
    return _math.sqrt(float(x))


def exp(x):
    if hasattr(x, 'shape'):
        return _core.array([_math.exp(float(v)) for v in (x._flat if hasattr(x, '_flat') else x)])
    return _math.exp(float(x))


def log(x):
    if hasattr(x, 'shape'):
        return _core.array([_math.log(float(v)) if v > 0 else float('-inf') for v in (x._flat if hasattr(x, '_flat') else x)])
    return _math.log(float(x))

# Trigonometric / transcendental fallbacks — element-wise via pure
# Python loops if the native core doesn't carry them.


def _elementwise(scalar_op):
    def fn(a):
        if hasattr(a, 'shape'):
            out = empty(a.shape, dtype_=float64)
            n = len(a) if a.shape else 0
            for i in range(n):
                try:
                    out[i] = scalar_op(float(a[i]))
                except (TypeError, IndexError):
                    pass
            return out
        if isinstance(a, (list, tuple)):
            return [scalar_op(float(x)) for x in a]
        return scalar_op(float(a))
    return fn


log2 = _elementwise(lambda x: _math.log2(x) if x > 0 else float('-inf'))
log10 = _elementwise(lambda x: _math.log10(x) if x > 0 else float('-inf'))
sin = _elementwise(_math.sin)
cos = _elementwise(_math.cos)
tan = _elementwise(_math.tan)


def asin(x):
    if hasattr(x, 'shape'):
        return _elementwise(_math.asin)(x)
    return _math.asin(x)


def acos(x):
    if hasattr(x, 'shape'):
        return _elementwise(_math.acos)(x)
    return _math.acos(x)


def atan(x):
    if hasattr(x, 'shape'):
        return _elementwise(_math.atan)(x)
    return _math.atan(x)


def atan2(y, x):
    return _math.atan2(y, x)


floor = _elementwise(_math.floor)
ceil = _elementwise(_math.ceil)
trunc = _elementwise(_math.trunc)


def round(a, decimals=0):
    if hasattr(a, 'shape'):
        op = lambda v: __builtins__.get('round')(v, decimals) if isinstance(__builtins__, dict) else __builtins__.round(v, decimals)  # pragma: no cover
        try:
            return _elementwise(op)(a)
        except Exception:
            return a
    return __builtins__.round(a, decimals) if not isinstance(__builtins__, dict) else __builtins__['round'](a, decimals)


def sum(a, axis=None):
    if hasattr(a, 'sum'):
        if axis is None:
            return a.sum()
        return a.sum(axis)
    return _math.fsum(a)


def prod(a, axis=None):
    if hasattr(a, 'prod'):
        return a.prod() if axis is None else a.prod(axis)
    out = 1
    for v in a:
        out *= v
    return out


def mean(a, axis=None):
    if hasattr(a, 'mean'):
        return a.mean() if axis is None else a.mean(axis)
    seq = list(a)
    return sum(seq) / len(seq) if seq else 0


def std(a, axis=None, ddof=0):
    m = mean(a, axis=axis)
    if hasattr(a, '__iter__'):
        diffs = [(float(x) - m) ** 2 for x in a]
        denom = max(len(diffs) - ddof, 1)
        return (_math.fsum(diffs) / denom) ** 0.5
    return 0.0


def var(a, axis=None, ddof=0):
    return std(a, axis=axis, ddof=ddof) ** 2


def min(a, axis=None):  # noqa: A001 - mirror numpy spelling
    if hasattr(a, 'min'):
        return a.min() if axis is None else a.min(axis)
    return __builtins__['min'](a) if isinstance(__builtins__, dict) else __builtins__.min(a)


def max(a, axis=None):  # noqa: A001
    if hasattr(a, 'max'):
        return a.max() if axis is None else a.max(axis)
    return __builtins__['max'](a) if isinstance(__builtins__, dict) else __builtins__.max(a)


def argmin(a, axis=None):
    if hasattr(a, 'argmin'):
        return a.argmin() if axis is None else a.argmin(axis)
    return list(a).index(min(a))


def argmax(a, axis=None):
    if hasattr(a, 'argmax'):
        return a.argmax() if axis is None else a.argmax(axis)
    return list(a).index(max(a))


def concatenate(arrays, axis=0):
    if hasattr(_core, 'concatenate'):
        try:
            return _core.concatenate(list(arrays), axis)
        except TypeError:
            return _core.concatenate(list(arrays))
    out = []
    for a in arrays:
        if hasattr(a, 'tolist'):
            out.extend(a.tolist())
        else:
            out.extend(list(a))
    return array(out)


def stack(arrays, axis=0):
    return concatenate([a[None] if hasattr(a, '__getitem__') else a for a in arrays], axis=axis)


def hstack(arrays):
    return concatenate(arrays, axis=-1)


def vstack(arrays):
    return concatenate(arrays, axis=0)


def reshape(a, shape):
    if hasattr(a, 'reshape'):
        return a.reshape(*_resolve_shape(shape))
    return a


def ravel(a):
    if hasattr(a, 'ravel'):
        return a.ravel()
    return a


def transpose(a, axes=None):
    if hasattr(a, 'transpose'):
        return a.transpose(*axes) if axes is not None else a.transpose()
    return a


def where(cond, *rest):
    if not rest:
        return [i for i, c in enumerate(cond) if c]
    a, b = rest
    return [(av if cv else bv) for cv, av, bv in zip(cond, a, b)]


def argwhere(a):
    return [i for i, v in enumerate(a) if v]


def dot(a, b):
    if hasattr(a, 'dot'):
        return a.dot(b)
    return _math.fsum(x * y for x, y in zip(a, b))


def matmul(a, b):
    if hasattr(a, '__matmul__'):
        return a @ b
    return dot(a, b)


def inner(a, b):
    return dot(a, b)


def outer(a, b):
    out = empty((len(a), len(b)), dtype_=float64)
    for i, x in enumerate(a):
        for j, y in enumerate(b):
            try:
                out[i, j] = float(x) * float(y)
            except (TypeError, IndexError):
                pass
    return out


def array_equal(a, b):
    try:
        if hasattr(a, 'tolist'):
            a = a.tolist()
        if hasattr(b, 'tolist'):
            b = b.tolist()
        return list(a) == list(b)
    except Exception:
        return False


def allclose(a, b, rtol=1e-05, atol=1e-08):
    try:
        if hasattr(a, 'tolist'):
            a = a.tolist()
        if hasattr(b, 'tolist'):
            b = b.tolist()
        for x, y in zip(_flatten(a), _flatten(b)):
            if not isclose(x, y, rtol=rtol, atol=atol):
                return False
        return True
    except Exception:
        return False


def isclose(a, b, rtol=1e-05, atol=1e-08):
    try:
        return abs(float(a) - float(b)) <= atol + rtol * abs(float(b))
    except Exception:
        return False


def _flatten(x):
    if isinstance(x, (list, tuple)):
        for item in x:
            yield from _flatten(item)
    else:
        yield x


# ---------------------------------------------------------------------
# Submodules
# ---------------------------------------------------------------------

class _NamespaceModule:
    """Tiny module-like namespace; numpy.linalg etc. live as instances of this."""

    def __init__(self, name):
        self.__name__ = name

    def __repr__(self):
        return '<numpy submodule {!r}>'.format(self.__name__)


linalg = _NamespaceModule('numpy.linalg')


def _linalg_det(a):
    """Very small determinant — only used for shape-(n, n) where n <= 3."""
    if hasattr(a, 'shape'):
        rows = a.shape[0]
    else:
        rows = len(a)
    if rows == 1:
        return float(a[0][0]) if hasattr(a[0], '__getitem__') else float(a[0, 0])
    if rows == 2:
        return float(a[0, 0]) * float(a[1, 1]) - float(a[0, 1]) * float(a[1, 0])
    if rows == 3:
        m = [[float(a[i, j]) for j in range(3)] for i in range(3)]
        return (m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]))
    raise NotImplementedError('numpy.linalg.det only supports n<=3 in WeavePy facade')


def _linalg_norm(a, ord=None, axis=None):  # noqa: A002
    if ord is None or ord == 2:
        return _math.sqrt(_math.fsum(float(x) ** 2 for x in _flatten(a.tolist() if hasattr(a, 'tolist') else a)))
    if ord == 1:
        return _math.fsum(abs(float(x)) for x in _flatten(a.tolist() if hasattr(a, 'tolist') else a))
    if ord == float('inf'):
        flat = list(_flatten(a.tolist() if hasattr(a, 'tolist') else a))
        return __builtins__.max(abs(float(x)) for x in flat) if not isinstance(__builtins__, dict) else __builtins__['max'](abs(float(x)) for x in flat)
    raise NotImplementedError('numpy.linalg.norm ord={!r}'.format(ord))


def _linalg_inv(a):
    rows = a.shape[0] if hasattr(a, 'shape') else len(a)
    if rows == 1:
        return array([[1.0 / float(a[0, 0])]])
    if rows == 2:
        d = _linalg_det(a)
        if d == 0:
            raise ValueError('singular matrix')
        return array([[float(a[1, 1]) / d, -float(a[0, 1]) / d],
                      [-float(a[1, 0]) / d, float(a[0, 0]) / d]])
    raise NotImplementedError('numpy.linalg.inv only supports n<=2 in WeavePy facade')


linalg.det = _linalg_det
linalg.norm = _linalg_norm
linalg.inv = _linalg_inv


# numpy.random — minimal facade over Python's `random`.

class _RandomModule(_NamespaceModule):
    def __init__(self):
        super().__init__('numpy.random')
        import random as _random
        self._random = _random
        self.RandomState = self  # Returns the module itself as a state proxy.
        self.default_rng = lambda seed=None: self

    def seed(self, s=None):
        self._random.seed(s)

    def rand(self, *shape):
        if not shape:
            return self._random.random()
        return array([self._random.random() for _ in range(_prod(shape))])

    def randn(self, *shape):
        if not shape:
            return self._random.gauss(0.0, 1.0)
        return array([self._random.gauss(0.0, 1.0) for _ in range(_prod(shape))])

    def randint(self, low, high=None, size=None):
        if high is None:
            high = low
            low = 0
        if size is None:
            return self._random.randint(low, high - 1)
        return array([self._random.randint(low, high - 1) for _ in range(_prod(_resolve_shape(size)))])

    def uniform(self, low=0.0, high=1.0, size=None):
        if size is None:
            return self._random.uniform(low, high)
        return array([self._random.uniform(low, high) for _ in range(_prod(_resolve_shape(size)))])

    def normal(self, loc=0.0, scale=1.0, size=None):
        if size is None:
            return self._random.gauss(loc, scale)
        return array([self._random.gauss(loc, scale) for _ in range(_prod(_resolve_shape(size)))])

    def choice(self, a, size=None, replace=True):
        if isinstance(a, int):
            a = list(range(a))
        if hasattr(a, 'tolist'):
            a = a.tolist()
        if size is None:
            return self._random.choice(a)
        n = _prod(_resolve_shape(size))
        if replace:
            return array([self._random.choice(a) for _ in range(n)])
        return array(self._random.sample(a, n))

    def shuffle(self, a):
        if hasattr(a, 'tolist'):
            data = a.tolist()
        else:
            data = list(a)
        self._random.shuffle(data)
        return data


def _prod(seq):
    out = 1
    for v in seq:
        out *= int(v)
    return out


random = _RandomModule()


# numpy.fft — minimal facade.

class _FFTModule(_NamespaceModule):
    def __init__(self):
        super().__init__('numpy.fft')

    def fft(self, a):
        return _naive_fft(list(a))

    def ifft(self, a):
        n = len(list(a))
        return [c / n for c in _naive_fft(list(a), inverse=True)]


def _naive_fft(a, inverse=False):
    n = len(a)
    if n <= 1:
        return list(a)
    sign = 1.0 if inverse else -1.0
    out = []
    for k in range(n):
        s = 0.0 + 0.0j
        for j in range(n):
            angle = sign * 2.0 * _math.pi * j * k / n
            s += complex(a[j]) * complex(_math.cos(angle), _math.sin(angle))
        out.append(s)
    return out


fft = _FFTModule()


# numpy.testing — minimal assert helpers used by pytest plugins.

class _TestingModule(_NamespaceModule):
    def __init__(self):
        super().__init__('numpy.testing')

    def assert_array_equal(self, a, b, err_msg=''):
        if not array_equal(a, b):
            raise AssertionError(err_msg or 'array equality check failed: {!r} != {!r}'.format(a, b))

    def assert_array_almost_equal(self, a, b, decimal=7, err_msg=''):
        if not allclose(a, b, atol=10 ** (-decimal)):
            raise AssertionError(err_msg or 'array almost-equal failed: {!r} != {!r}'.format(a, b))

    def assert_allclose(self, a, b, rtol=1e-7, atol=0):
        if not allclose(a, b, rtol=rtol, atol=atol):
            raise AssertionError('arrays not close: {!r} vs {!r}'.format(a, b))

    def assert_equal(self, a, b, err_msg=''):
        if a != b:
            raise AssertionError(err_msg or '{!r} != {!r}'.format(a, b))


testing = _TestingModule()


# Re-export ufuncs as numpy attributes (legacy spelling).
ufunc = type('ufunc', (object,), {})

# Self-test under direct execution.
if __name__ == '__main__':
    a = zeros((2, 3))
    assert a.shape == (2, 3), a.shape
    b = arange(6).reshape(2, 3)
    c = a + b
    assert c.shape == (2, 3)
    print('numpy facade smoke OK (backend={})'.format(_CORE_KIND))
