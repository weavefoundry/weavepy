"""``_numpy_pure`` — pure-Python ndarray fallback for numpy facade.

When the bundled ``_numpylike`` C extension isn't compiled into the
WeavePy binary (e.g. on platforms where the build harness hasn't been
run yet), :mod:`numpy` falls back to this module so user code keeps
working — just slower.

Implements a 1- and 2-D dense ndarray with the API surface most numpy
consumers reach for:

* ``shape``, ``dtype``, ``ndim``, ``size``, ``itemsize``
* element-wise ``+``, ``-``, ``*``, ``/``, ``//``, ``%``, ``**``,
  unary ``-``, ``abs``
* indexing (``a[i]``, ``a[i, j]``, ``a[i:j]``, ``a[:, j]``)
* reductions (``sum``, ``prod``, ``mean``, ``min``, ``max``,
  ``argmin``, ``argmax``)
* shape ops (``reshape``, ``ravel``, ``transpose``, ``T``)
* linear algebra primitives (``dot``, ``matmul``)
* ``tolist()`` / ``__iter__`` / ``__len__`` / ``__repr__``

This is intentionally minimal — performance-sensitive code should
build with the native extension. The fallback exists to keep the
"drop-in" contract: ``import numpy`` always succeeds.
"""

import math as _math
import struct as _struct


__all__ = ['NDArray', 'array', 'zeros', 'ones', 'empty', 'arange',
           'concatenate']


def _struct_code(dtype):
    """struct format char + element coercion for a dtype name — covers
    the dtype spellings the pure core actually produces ('f8', 'int64',
    'float64', …)."""
    name = str(dtype).lstrip('<>=|')
    if name.startswith('u'):
        return 'Q', int
    if name.startswith(('i', 'l', 'q')):
        return 'q', int
    return 'd', float


def _fsum(iterable):
    """Like ``math.fsum`` but accepts generators by materialising them."""
    if hasattr(_math, 'fsum') and isinstance(iterable, (list, tuple)):
        return _math.fsum(iterable)
    return sum(iterable)


def _prod(seq):
    out = 1
    for v in seq:
        out *= int(v)
    return out


def _flatten(data):
    if isinstance(data, (list, tuple)):
        for item in data:
            yield from _flatten(item)
    else:
        yield data


def _infer_shape(data):
    if isinstance(data, (list, tuple)):
        if not data:
            return (0,)
        first = data[0]
        if isinstance(first, (list, tuple)):
            inner = _infer_shape(first)
            return (len(data),) + inner
        return (len(data),)
    return ()


class NDArray:
    """Minimal pure-Python n-dimensional array (1-D or 2-D)."""

    __slots__ = ('_flat', '_shape', '_dtype', '_strides')

    def __init__(self, flat, shape, dtype='float64'):
        self._flat = list(flat)
        self._shape = tuple(shape)
        self._dtype = dtype
        self._strides = self._calc_strides()

    def _calc_strides(self):
        out = []
        s = 1
        for dim in reversed(self._shape):
            out.append(s)
            s *= dim
        return tuple(reversed(out))

    # --- attributes

    @property
    def shape(self):
        return self._shape

    @property
    def ndim(self):
        return len(self._shape)

    @property
    def size(self):
        return _prod(self._shape) if self._shape else 1

    @property
    def dtype(self):
        return self._dtype

    @property
    def itemsize(self):
        return 8  # float64-ish default; minor — pure fallback.

    @property
    def nbytes(self):
        return self.size * self.itemsize

    @property
    def T(self):
        return self.transpose()

    @property
    def flags(self):
        return _Flags(self)

    @property
    def ctypes(self):
        return _CTypes(self)

    # --- conversions

    def tolist(self):
        if self.ndim <= 1:
            return list(self._flat)
        if self.ndim == 2:
            r, c = self._shape
            return [list(self._flat[i * c:(i + 1) * c]) for i in range(r)]
        return list(self._flat)

    def __iter__(self):
        if self.ndim <= 1:
            return iter(self._flat)
        if self.ndim == 2:
            r, c = self._shape
            return iter([NDArray(self._flat[i * c:(i + 1) * c], (c,),
                                 dtype=self._dtype)
                         for i in range(r)])
        return iter(self._flat)

    def __len__(self):
        if not self._shape:
            return 0
        return self._shape[0]

    def __repr__(self):
        return 'array({!r}, dtype={!r})'.format(self.tolist(), self._dtype)

    # --- shape ops

    def reshape(self, *shape):
        if len(shape) == 1 and isinstance(shape[0], (tuple, list)):
            shape = tuple(shape[0])
        new = tuple(int(d) for d in shape)
        if -1 in new:
            known = _prod(d for d in new if d != -1)
            if known == 0:
                raise ValueError('cannot reshape array of size 0 with -1')
            new = tuple(self.size // known if d == -1 else d for d in new)
        if _prod(new) != self.size:
            raise ValueError('cannot reshape array of size {} into {}'.format(self.size, new))
        return NDArray(self._flat, new, dtype=self._dtype)

    def ravel(self):
        return NDArray(self._flat, (self.size,), dtype=self._dtype)

    def transpose(self, *axes):
        if self.ndim < 2:
            return self
        if self.ndim == 2:
            r, c = self._shape
            out = [0.0] * (r * c)
            for i in range(r):
                for j in range(c):
                    out[j * r + i] = self._flat[i * c + j]
            return NDArray(out, (c, r), dtype=self._dtype)
        return self

    # --- arithmetic

    def _binop(self, other, op):
        if isinstance(other, NDArray):
            if other.shape == self.shape:
                return NDArray([op(a, b) for a, b in zip(self._flat, other._flat)],
                                self.shape, dtype=self._dtype)
            # Broadcast scalar-shape.
            if other.size == 1:
                v = other._flat[0]
                return NDArray([op(a, v) for a in self._flat], self.shape, dtype=self._dtype)
            if self.size == 1:
                v = self._flat[0]
                return NDArray([op(v, b) for b in other._flat], other.shape, dtype=self._dtype)
            raise ValueError('shape mismatch: {} vs {}'.format(self.shape, other.shape))
        # Scalar.
        try:
            v = float(other)
        except (TypeError, ValueError):
            return NotImplemented
        return NDArray([op(a, v) for a in self._flat], self.shape, dtype=self._dtype)

    def __add__(self, other):
        return self._binop(other, lambda a, b: a + b)

    def __radd__(self, other):
        return self.__add__(other)

    def __sub__(self, other):
        return self._binop(other, lambda a, b: a - b)

    def __rsub__(self, other):
        return self._binop(other, lambda a, b: b - a)

    def __mul__(self, other):
        return self._binop(other, lambda a, b: a * b)

    def __rmul__(self, other):
        return self.__mul__(other)

    def __truediv__(self, other):
        return self._binop(other, lambda a, b: a / b)

    def __rtruediv__(self, other):
        return self._binop(other, lambda a, b: b / a)

    def __floordiv__(self, other):
        return self._binop(other, lambda a, b: a // b)

    def __mod__(self, other):
        return self._binop(other, lambda a, b: a % b)

    def __pow__(self, other):
        return self._binop(other, lambda a, b: a ** b)

    def __neg__(self):
        return NDArray([-a for a in self._flat], self.shape, dtype=self._dtype)

    def __abs__(self):
        return NDArray([abs(a) for a in self._flat], self.shape, dtype=self._dtype)

    def __matmul__(self, other):
        return self.dot(other)

    def __eq__(self, other):
        if isinstance(other, NDArray):
            return self.tolist() == other.tolist()
        return self._flat == list(other) if hasattr(other, '__iter__') else NotImplemented

    def __hash__(self):
        return id(self)

    # --- indexing

    def __getitem__(self, key):
        if isinstance(key, int):
            if self.ndim == 1:
                return self._flat[key]
            if self.ndim == 2:
                _, c = self._shape
                return NDArray(self._flat[key * c:(key + 1) * c], (c,), dtype=self._dtype)
        if isinstance(key, tuple) and len(key) == 2 and self.ndim == 2:
            i, j = key
            _, c = self._shape
            if isinstance(i, int) and isinstance(j, int):
                return self._flat[i * c + j]
        if isinstance(key, slice) and self.ndim == 1:
            return NDArray(self._flat[key], (len(self._flat[key]),), dtype=self._dtype)
        if isinstance(key, slice) and self.ndim == 2:
            r, c = self._shape
            rows = range(*key.indices(r))
            flat = []
            for i in rows:
                flat.extend(self._flat[i * c:(i + 1) * c])
            return NDArray(flat, (len(rows), c), dtype=self._dtype)
        return self._flat[key]

    def __setitem__(self, key, value):
        if isinstance(key, int):
            if self.ndim == 1:
                self._flat[key] = value
                return
            if self.ndim == 2:
                _, c = self._shape
                if hasattr(value, '__iter__'):
                    for j, v in enumerate(value):
                        self._flat[key * c + j] = v
                else:
                    for j in range(c):
                        self._flat[key * c + j] = value
                return
        if isinstance(key, tuple) and len(key) == 2 and self.ndim == 2:
            i, j = key
            _, c = self._shape
            self._flat[i * c + j] = value
            return
        self._flat[key] = value

    # --- reductions

    def sum(self, axis=None):
        if axis is None or self.ndim <= 1:
            return _fsum(self._flat)
        if self.ndim == 2 and axis == 0:
            r, c = self._shape
            return NDArray(
                [_fsum(self._flat[i * c + j] for i in range(r)) for j in range(c)],
                (c,), dtype=self._dtype,
            )
        if self.ndim == 2 and axis == 1:
            r, c = self._shape
            return NDArray(
                [_fsum(self._flat[i * c:(i + 1) * c]) for i in range(r)],
                (r,), dtype=self._dtype,
            )
        raise NotImplementedError('sum(axis={}) for ndim={}'.format(axis, self.ndim))

    def prod(self, axis=None):  # noqa: ARG002
        out = 1
        for v in self._flat:
            out *= v
        return out

    def mean(self, axis=None):  # noqa: ARG002
        if not self._flat:
            return 0.0
        return _fsum(self._flat) / len(self._flat)

    def min(self, axis=None):  # noqa: ARG002
        return min(self._flat)

    def max(self, axis=None):  # noqa: ARG002
        return max(self._flat)

    def argmin(self, axis=None):  # noqa: ARG002
        m = self._flat[0]
        idx = 0
        for i, v in enumerate(self._flat):
            if v < m:
                m = v
                idx = i
        return idx

    def argmax(self, axis=None):  # noqa: ARG002
        m = self._flat[0]
        idx = 0
        for i, v in enumerate(self._flat):
            if v > m:
                m = v
                idx = i
        return idx

    def fill(self, value):
        for i in range(len(self._flat)):
            self._flat[i] = value

    def astype(self, dtype, copy=True):  # noqa: ARG002
        return NDArray(self._flat, self.shape, dtype=str(dtype))

    def copy(self):
        return NDArray(self._flat, self.shape, dtype=self._dtype)

    # --- buffer protocol / pickling (test_pickle's test_buffers_numpy)

    def __buffer__(self, flags):  # noqa: ARG002
        # PEP 688 export: pack the flat storage into real bytes so
        # `memoryview(arr)` / `pickle.PickleBuffer(arr)` work. The VM
        # records *this array* as the view's exporter, which is how the
        # zero-copy unpickle path below finds its way back to the
        # original storage.
        code, coerce = _struct_code(self._dtype)
        packed = _struct.pack('<%d%s' % (len(self._flat), code),
                              *[coerce(v) for v in self._flat])
        return memoryview(packed)

    def __reduce_ex__(self, protocol):
        # Mirrors numpy's pickle-5 support: protocol >= 5 routes the
        # data through a PickleBuffer, so a `buffer_callback` can take
        # it out-of-band and unpickling with `buffers=` reconstructs an
        # array *sharing* this one's storage (pickletester's
        # check_no_copy compares `ctypes.data`). Older protocols — and
        # in-band buffers — rebuild from a plain nested list.
        if protocol >= 5:
            import pickle
            return (_from_buffer,
                    (pickle.PickleBuffer(self), str(self._dtype),
                     self._shape))
        return (_from_list, (self.tolist(), str(self._dtype)))

    def dot(self, other):
        if isinstance(other, NDArray):
            if self.ndim == 1 and other.ndim == 1:
                return _fsum(a * b for a, b in zip(self._flat, other._flat))
            if self.ndim == 2 and other.ndim == 2:
                r, k = self._shape
                k2, c = other._shape
                if k != k2:
                    raise ValueError('matmul shape mismatch: {} vs {}'.format(self.shape, other.shape))
                out = [0.0] * (r * c)
                for i in range(r):
                    for j in range(c):
                        s = 0.0
                        for kk in range(k):
                            s += self._flat[i * k + kk] * other._flat[kk * c + j]
                        out[i * c + j] = s
                return NDArray(out, (r, c), dtype=self._dtype)
        return _fsum(a * b for a, b in zip(self._flat, other))


class _Flags:
    """`ndarray.flags` lookalike. The pure core always materialises a
    dense C-order copy (slices/transposes included), so every array
    reports C-contiguous."""

    __slots__ = ('c_contiguous', 'f_contiguous', 'writeable')

    def __init__(self, arr):
        self.c_contiguous = True
        self.f_contiguous = arr.ndim <= 1
        self.writeable = True


class _CTypes:
    """`ndarray.ctypes` lookalike: `data` stands in for the data
    pointer — the identity of the flat storage list, so arrays sharing
    storage report the same "address" and copies report different
    ones."""

    __slots__ = ('data',)

    def __init__(self, arr):
        self.data = id(arr._flat)


def _from_list(data, dtype):
    """Unpickle an array serialized in-band as a nested list."""
    return array(data, dtype=dtype)


def _from_buffer(buf, dtype, shape):
    """Unpickle a protocol-5 array. When `buf` is the very PickleBuffer
    the pickler handed to `buffer_callback` (out-of-band, same
    process), its exported view leads back to the source NDArray —
    share that storage so the round-trip is zero-copy, like numpy's
    `_frombuffer` over the original memory. Otherwise (in-band, or
    cross-process) decode the packed bytes."""
    with memoryview(buf) as m:
        src = m.obj
        if isinstance(src, NDArray):
            out = NDArray.__new__(NDArray)
            out._flat = src._flat
            out._shape = tuple(shape)
            out._dtype = dtype
            out._strides = out._calc_strides()
            return out
        code, _coerce = _struct_code(dtype)
        n = _prod(shape) if shape else 1
        vals = list(_struct.unpack('<%d%s' % (n, code), m.tobytes()))
        return NDArray(vals, shape, dtype=dtype)


def array(data, dtype='float64'):
    if isinstance(data, NDArray):
        return NDArray(data._flat, data._shape, dtype=dtype)
    shape = _infer_shape(data)
    flat = list(_flatten(data))
    return NDArray(flat, shape, dtype=dtype)


def zeros(shape, dtype='float64'):
    if isinstance(shape, int):
        shape = (shape,)
    n = _prod(shape) if shape else 1
    return NDArray([0.0] * n, shape, dtype=dtype)


def ones(shape, dtype='float64'):
    if isinstance(shape, int):
        shape = (shape,)
    n = _prod(shape) if shape else 1
    return NDArray([1.0] * n, shape, dtype=dtype)


def empty(shape, dtype='float64'):
    return zeros(shape, dtype=dtype)


def arange(start, stop=None, step=1, dtype='int64'):
    if stop is None:
        stop = start
        start = 0
    out = []
    v = start
    while (step > 0 and v < stop) or (step < 0 and v > stop):
        out.append(v)
        v += step
    return NDArray(out, (len(out),), dtype=dtype)


def concatenate(arrays, axis=0):  # noqa: ARG001
    flat = []
    for a in arrays:
        if isinstance(a, NDArray):
            flat.extend(a._flat)
        else:
            flat.extend(list(a))
    return NDArray(flat, (len(flat),))
