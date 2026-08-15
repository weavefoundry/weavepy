"""``array`` — efficient arrays of numeric values.

Byte-backed implementation: the array's contents live in a single
``bytearray`` (``self._buf``) holding the packed items, exactly like
CPython's C ``arrayobject``. Element access packs/unpacks through
``struct`` on demand. Backing the storage with real bytes (rather than a
Python list) is what makes the PEP 688 buffer protocol *write-through*:
``memoryview(a)`` and ``a.__buffer__(...)`` expose ``self._buf`` directly,
so ``f.readinto(a)`` and ``struct.pack_into(a, ...)`` mutate the array in
place (``test_array``/``test_io.test_readinto_array``).
"""

import struct as _struct
import sys as _sys


__all__ = ['array', 'ArrayType', 'typecodes', '_array_reconstructor']


typecodes = 'bBuwhHiIlLqQfd'

# Unicode-valued typecodes: their items are length-1 ``str`` code points,
# not numbers. ``'u'`` is the legacy ``wchar_t`` array (deprecated since
# 3.3, warning since 3.13); ``'w'`` is the Py_UCS4 array added in 3.13.
_UNICODE_TYPECODES = ('u', 'w')

# ``'u'`` stores C ``wchar_t``: 2 bytes on Windows, 4 everywhere else.
_WCHAR_SIZE = 2 if _sys.platform == 'win32' else 4

# Map type code -> (struct format, item size).
_TYPECODES = {
    'b': ('b', 1),
    'B': ('B', 1),
    'u': ('H' if _WCHAR_SIZE == 2 else 'I', _WCHAR_SIZE),
    'w': ('I', 4),
    'h': ('h', 2),
    'H': ('H', 2),
    'i': ('i', 4),
    'I': ('I', 4),
    # C `long` is platform-sized (8 bytes on LP64) — ctypes'
    # test_int_from_address overlays c_long on array('l') storage.
    'l': ('l', _struct.calcsize('l')),
    'L': ('L', _struct.calcsize('L')),
    'q': ('q', 8),
    'Q': ('Q', 8),
    'f': ('f', 4),
    'd': ('d', 8),
}

_INT_TYPECODES = 'bBhHiIlLqQ'
_FLOAT_TYPECODES = 'fd'

# Inclusive (min, max) per integer typecode, derived from the struct
# format's signedness and width — assignment outside raises OverflowError
# exactly like CPython's per-type setters.
_INT_RANGES = {}
for _tc in _INT_TYPECODES:
    _f, _size = _TYPECODES[_tc]
    if _f.islower():
        _INT_RANGES[_tc] = (-(1 << (8 * _size - 1)), (1 << (8 * _size - 1)) - 1)
    else:
        _INT_RANGES[_tc] = (0, (1 << (8 * _size)) - 1)
del _tc, _f, _size

_MISSING = object()


def _make_array(typecode):
    """Internal bare constructor: no typecode re-validation and — key for
    'u' — no re-firing of the DeprecationWarning on every slice/repeat."""
    self = object.__new__(array)
    fmt, size = _TYPECODES[typecode]
    self._typecode = typecode
    self._fmt = fmt
    self._itemsize = size
    self._buf = bytearray()
    return self


class _array_iterator:
    """CPython's ``arrayiterator``: index-based, so an iterator that never
    saw a failed ``next()`` picks up items appended later; one that raised
    StopIteration drops its array reference and stays exhausted
    (test_array.test_exhausted_iterator / gh-128961)."""

    __slots__ = ('_ao', '_index')

    def __init__(self, ao):
        self._ao = ao
        self._index = 0

    def __iter__(self):
        return self

    def __next__(self):
        ao = self._ao
        if ao is None:
            raise StopIteration
        i = self._index
        if i < len(ao):
            self._index = i + 1
            return ao._unpack(i)
        self._ao = None
        raise StopIteration

    def __reduce__(self):
        if self._ao is not None:
            return (iter, (self._ao,), self._index)
        # Exhausted: unpickles to an iterator over an empty tuple
        # (CPython `Py_BuildValue("N(())", iter)`).
        return (iter, ((),))

    def __setstate__(self, state):
        if self._ao is not None:
            index = state.__index__()
            size = len(self._ao)
            if index < 0:
                index = 0
            elif index > size:
                index = size
            self._index = index


class array:
    # CPython's array is a fixed C struct: instances have no __dict__
    # (subclasses without __slots__ regain one), and typecode/itemsize
    # are read-only getsets.
    __slots__ = ('_typecode', '_fmt', '_itemsize', '_buf', '__weakref__')

    # The C type carries Py_TPFLAGS_SEQUENCE, so `case [..]:` patterns
    # match arrays (PEP 634); WeavePy's VM reads the flag off this private
    # marker (the same key ABCMeta stows __abc_tpflags__ under).
    _abc_collection_flags = 1 << 5  # Py_TPFLAGS_SEQUENCE

    def __class_getitem__(cls, item):
        # CPython's C `array.array` exposes `__class_getitem__ =
        # Py_GenericAlias` (test_genericalias generic_types sweep).
        import types

        return types.GenericAlias(cls, item)

    def __new__(cls, typecode=_MISSING, initializer=_MISSING, *rest, **kwargs):
        if typecode is _MISSING:
            raise TypeError(
                "array() takes at least 1 argument (0 given)"
            )
        if rest:
            raise TypeError(
                "array() takes at most 2 arguments (%d given)" % (2 + len(rest))
            )
        # CPython rejects keywords only for the exact base type; subclasses
        # route extra keywords to their own __init__ (SF bug #1486663).
        if kwargs and cls is array:
            raise TypeError("array() takes no keyword arguments")
        if not isinstance(typecode, str) or len(typecode) != 1:
            raise TypeError(
                "array() argument 1 must be a unicode character, not %s"
                % type(typecode).__name__
            )
        if typecode not in _TYPECODES:
            raise ValueError(
                "bad typecode (must be b, B, u, w, h, H, i, I, l, L, q, Q, f or d)"
            )
        if typecode == 'u':
            import warnings

            warnings.warn(
                "The 'u' type code is deprecated and "
                "will be removed in Python 3.16",
                DeprecationWarning,
                stacklevel=2,
            )
        self = object.__new__(cls)
        fmt, size = _TYPECODES[typecode]
        self._typecode = typecode
        self._fmt = fmt
        self._itemsize = size
        self._buf = bytearray()
        if initializer is _MISSING:
            return self
        if typecode in _UNICODE_TYPECODES:
            if isinstance(initializer, str):
                self.fromunicode(initializer)
            elif isinstance(initializer, array) and \
                    initializer._typecode in _UNICODE_TYPECODES:
                for ch in initializer:
                    self.append(ch)
            elif isinstance(initializer, (bytes, bytearray)):
                self.frombytes(bytes(initializer))
            else:
                for item in initializer:
                    self.append(item)
        else:
            if isinstance(initializer, str):
                raise TypeError(
                    "cannot use a str to initialize an array with typecode '%s'"
                    % typecode
                )
            if isinstance(initializer, array) and \
                    initializer._typecode in _UNICODE_TYPECODES:
                raise TypeError(
                    "cannot use a unicode array to initialize an array with "
                    "typecode '%s'" % typecode
                )
            if isinstance(initializer, (bytes, bytearray)):
                self.frombytes(bytes(initializer))
            elif isinstance(initializer, array):
                if initializer._typecode == typecode:
                    self._buf[:] = initializer._buf
                else:
                    for v in initializer:
                        self.append(v)
            else:
                for item in initializer:
                    self.append(item)
        return self

    def __init__(self, *args, **kwargs):
        # Construction happens entirely in __new__ (like the C type);
        # `array.array.__init__(self)` from subclasses is a no-op.
        pass

    # -- read-only metadata (CPython getsets) -----------------------------

    @property
    def typecode(self):
        return self._typecode

    @property
    def itemsize(self):
        return self._itemsize

    # -- internal pack/unpack helpers ------------------------------------

    def _coerce(self, value):
        """Validate + convert one item, with CPython's error discipline:
        TypeError for wrong types (float into an int array, non-str into a
        unicode array), OverflowError for out-of-range integers."""
        tc = self._typecode
        if tc in _UNICODE_TYPECODES:
            if not isinstance(value, str) or len(value) != 1:
                raise TypeError('array item must be a unicode character')
            return value
        if tc in _INT_TYPECODES:
            if not isinstance(value, int):
                index = getattr(type(value), '__index__', None)
                if index is None:
                    raise TypeError(
                        "'%s' object cannot be interpreted as an integer"
                        % type(value).__name__
                    )
                value = index(value)
                if not isinstance(value, int):
                    raise TypeError('__index__ returned non-int')
            lo, hi = _INT_RANGES[tc]
            if value < lo:
                if lo == 0:
                    raise OverflowError(
                        "can't convert negative value to unsigned int"
                    )
                raise OverflowError('signed integer is less than minimum')
            if value > hi:
                raise OverflowError('signed integer is greater than maximum'
                                    if lo != 0 else
                                    'unsigned integer is greater than maximum')
            return value
        # float typecodes
        if isinstance(value, float):
            return value
        if isinstance(value, int):
            return float(value)
        tofloat = getattr(type(value), '__float__', None)
        if tofloat is None:
            raise TypeError(
                'must be real number, not %s' % type(value).__name__
            )
        value = tofloat(value)
        if not isinstance(value, float):
            raise TypeError('__float__ returned non-float')
        return value

    def _pack(self, value):
        value = self._coerce(value)
        if self._typecode in _UNICODE_TYPECODES:
            return _struct.pack(self._fmt, ord(value))
        try:
            return _struct.pack(self._fmt, value)
        except (OverflowError, _struct.error):
            raise OverflowError('array item out of range') from None

    def _unpack(self, index):
        off = index * self._itemsize
        value = _struct.unpack_from(self._fmt, self._buf, off)[0]
        if self._typecode in _UNICODE_TYPECODES:
            return chr(value)
        return value

    def _normalize_index(self, index):
        if not isinstance(index, int):
            toindex = getattr(type(index), '__index__', None)
            if toindex is None:
                raise TypeError(
                    "'%s' object cannot be interpreted as an integer"
                    % type(index).__name__
                )
            index = toindex(index)
        n = len(self)
        if index < 0:
            index += n
        if index < 0 or index >= n:
            raise IndexError('array index out of range')
        return index

    # -- mutating sequence API ------------------------------------------

    def append(self, value):
        self._buf += self._pack(value)

    def extend(self, iterable):
        if isinstance(iterable, array):
            if iterable._typecode != self._typecode:
                raise TypeError(
                    "can only extend with array of same kind"
                )
            self._buf += iterable._buf
            return
        for v in iterable:
            self.append(v)

    def insert(self, index, value):
        packed = self._pack(value)
        if not isinstance(index, int):
            index = index.__index__()
        n = len(self)
        if index < 0:
            index += n
            if index < 0:
                index = 0
        elif index > n:
            index = n
        off = index * self._itemsize
        self._buf[off:off] = packed

    def pop(self, index=-1):
        if len(self) == 0:
            raise IndexError('pop from empty array')
        index = self._normalize_index(index)
        value = self._unpack(index)
        off = index * self._itemsize
        del self._buf[off:off + self._itemsize]
        return value

    def remove(self, value):
        idx = self.index(value)
        off = idx * self._itemsize
        del self._buf[off:off + self._itemsize]

    def clear(self):
        del self._buf[:]

    def reverse(self):
        n = len(self)
        size = self._itemsize
        items = [bytes(self._buf[i * size:(i + 1) * size]) for i in range(n)]
        items.reverse()
        self._buf[:] = b''.join(items)

    def byteswap(self):
        # Reverse the byte order of every item in place. CPython supports this
        # for 1/2/4/8-byte items only (RuntimeError otherwise); it's how code
        # reading big-endian binary data flips it to native order
        # (`datetimetester`'s tzfile `ZoneInfo.fromfile`).
        size = self._itemsize
        if size not in (1, 2, 4, 8):
            raise RuntimeError("don't know how to byteswap this array type")
        if size == 1:
            return
        buf = self._buf
        for off in range(0, len(buf), size):
            buf[off:off + size] = buf[off:off + size][::-1]

    # -- non-mutating queries -------------------------------------------

    def count(self, value):
        return sum(1 for v in self if v == value)

    def index(self, value, *args):
        # Support optional start/stop like list.index.
        n = len(self)
        start = 0
        stop = n
        if args:
            start = args[0].__index__()
            if start < 0:
                start = max(n + start, 0)
            if len(args) > 1:
                stop = args[1].__index__()
                if stop < 0:
                    stop += n
                stop = min(stop, n)
        for i in range(start, stop):
            if self._unpack(i) == value:
                return i
        raise ValueError('array.index(x): x not in array')

    def tolist(self):
        return [self._unpack(i) for i in range(len(self))]

    def buffer_info(self):
        # CPython returns the real data pointer — ctypes tests build
        # `c_int.from_address(a.buffer_info()[0])` on it.
        import _ctypes_native
        return (_ctypes_native.addressof_buffer(self._buf), len(self))

    # -- bytes / file / unicode conversions -----------------------------

    def frombytes(self, blob):
        if isinstance(blob, (bytes, bytearray)):
            data = bytes(blob)
        elif isinstance(blob, str):
            raise TypeError("a bytes-like object is required, not 'str'")
        else:
            try:
                data = bytes(memoryview(blob))
            except TypeError:
                raise TypeError(
                    "a bytes-like object is required, not '%s'"
                    % type(blob).__name__
                ) from None
        if len(data) % self._itemsize:
            raise ValueError('bytes length not a multiple of item size')
        self._buf += data

    def tobytes(self):
        return bytes(self._buf)

    def fromlist(self, seq):
        if not isinstance(seq, list):
            raise TypeError('arg must be list')
        # All-or-nothing: pack into a scratch buffer first.
        scratch = bytearray()
        for v in seq:
            scratch += self._pack(v)
        self._buf += scratch

    def fromfile(self, fp, n):
        need = n * self._itemsize
        data = fp.read(need)
        if len(data) < need:
            self.frombytes(data[:len(data) - len(data) % self._itemsize])
            raise EOFError("read() didn't return enough bytes")
        self.frombytes(data)

    def tofile(self, fp):
        fp.write(self.tobytes())

    def fromunicode(self, s):
        if self._typecode not in _UNICODE_TYPECODES:
            raise ValueError("fromunicode() may only be called on "
                             "unicode type arrays")
        if not isinstance(s, str):
            raise TypeError(
                'fromunicode() argument must be str, not %s'
                % type(s).__name__
            )
        for ch in s:
            self.append(ch)

    def tounicode(self):
        if self._typecode not in _UNICODE_TYPECODES:
            raise ValueError("tounicode() may only be called on "
                             "unicode type arrays")
        return ''.join(self._unpack(i) for i in range(len(self)))

    # -- PEP 688 buffer protocol ----------------------------------------

    def __buffer__(self, flags):
        # Expose the live storage so consumers read/write *through* to the
        # array (CPython's C-level buffer export). ``self._buf`` is the
        # array's own bytearray, so ``memoryview(self._buf)`` shares it.
        # CPython's export carries the array's item format —
        # ``memoryview(array('i', ...))`` has format 'i'/itemsize 4
        # (test_memoryview's Array* classes) — so cast the byte view to the
        # typecode when the view layer supports it ('u'/'w' fall back raw).
        mv = memoryview(self._buf)
        try:
            return mv.cast(self._typecode)
        except (ValueError, TypeError):
            # 'u'/'w' are real CPython export formats that `cast` refuses
            # (struct doesn't know them). Retype the raw view so the format
            # survives — comparisons must see 'u', not 'B'
            # (test_buffer.test_memoryview_compare_special_cases_…_u_type_code).
            try:
                return mv._weavepy_with_format(self._typecode, self._itemsize)
            except (AttributeError, ValueError, TypeError):
                return mv

    # -- container protocol ---------------------------------------------

    def __len__(self):
        return len(self._buf) // self._itemsize

    def __iter__(self):
        return _array_iterator(self)

    def __getitem__(self, key):
        if isinstance(key, slice):
            out = _make_array(self._typecode)
            indices = range(*key.indices(len(self)))
            size = self._itemsize
            chunks = []
            for i in indices:
                chunks.append(bytes(self._buf[i * size:(i + 1) * size]))
            out._buf = bytearray(b''.join(chunks))
            return out
        return self._unpack(self._normalize_index(key))

    def __setitem__(self, key, value):
        size = self._itemsize
        if isinstance(key, slice):
            start, stop, step = key.indices(len(self))
            indices = list(range(start, stop, step))
            if isinstance(value, array):
                if value._typecode != self._typecode:
                    raise TypeError("bad argument type for built-in operation")
                packed = [bytes(value._buf[i * size:(i + 1) * size])
                          for i in range(len(value))]
            else:
                packed = [self._pack(v) for v in value]
            if step == 1:
                lo = start * size
                hi = lo + len(indices) * size
                self._buf[lo:hi] = b''.join(packed)
            else:
                if len(packed) != len(indices):
                    raise ValueError(
                        "attempt to assign sequence of size %d to extended "
                        "slice of size %d" % (len(packed), len(indices))
                    )
                for i, chunk in zip(indices, packed):
                    self._buf[i * size:(i + 1) * size] = chunk
            return
        # Pack the value *before* bounds-checking the index: the value's
        # __index__/__float__ can mutate the array (gh-142555), and CPython
        # re-checks the size afterwards, so a shrunken array raises
        # IndexError rather than writing out of bounds.
        packed = self._pack(value)
        index = self._normalize_index(key)
        self._buf[index * size:(index + 1) * size] = packed

    def __delitem__(self, key):
        size = self._itemsize
        if isinstance(key, slice):
            indices = list(range(*key.indices(len(self))))
            for i in sorted(indices, reverse=True):
                del self._buf[i * size:(i + 1) * size]
            return
        index = self._normalize_index(key)
        del self._buf[index * size:(index + 1) * size]

    def __contains__(self, value):
        for v in self:
            if v == value:
                return True
        return False

    def __add__(self, other):
        if not isinstance(other, array) or other._typecode != self._typecode:
            raise TypeError("can only append array (not \"%s\") to array"
                            % type(other).__name__)
        out = _make_array(self._typecode)
        out._buf = bytearray(self._buf) + other._buf
        return out

    def __iadd__(self, other):
        if not isinstance(other, array) or other._typecode != self._typecode:
            raise TypeError("can only extend array with array of same kind")
        self._buf += other._buf
        return self

    def _repeat_count(self, n):
        if not isinstance(n, int):
            index = getattr(type(n), '__index__', None)
            if index is None:
                raise TypeError(
                    "can't multiply sequence by non-int of type '%s'"
                    % type(n).__name__
                )
            n = index(n)
        return max(n, 0)

    def __mul__(self, n):
        n = self._repeat_count(n)
        out = _make_array(self._typecode)
        try:
            out._buf = bytearray(self._buf) * n
        except (OverflowError, MemoryError):
            # CPython's repeat allocates count*itemsize and reports
            # MemoryError when that exceeds the address space.
            raise MemoryError from None
        return out

    __rmul__ = __mul__

    def __imul__(self, n):
        n = self._repeat_count(n)
        try:
            self._buf *= n
        except (OverflowError, MemoryError):
            raise MemoryError from None
        return self

    def __repr__(self):
        if not len(self):
            return "array('%s')" % self._typecode
        if self._typecode in _UNICODE_TYPECODES:
            return "array('%s', %r)" % (self._typecode, self.tounicode())
        return "array('%s', %r)" % (self._typecode, self.tolist())

    def __eq__(self, other):
        if not isinstance(other, array):
            return NotImplemented
        return self.tolist() == other.tolist()

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __lt__(self, other):
        if not isinstance(other, array):
            return NotImplemented
        return self.tolist() < other.tolist()

    def __le__(self, other):
        if not isinstance(other, array):
            return NotImplemented
        return self.tolist() <= other.tolist()

    def __gt__(self, other):
        if not isinstance(other, array):
            return NotImplemented
        return self.tolist() > other.tolist()

    def __ge__(self, other):
        if not isinstance(other, array):
            return NotImplemented
        return self.tolist() >= other.tolist()

    # -- pickling (array_reduce_ex) -------------------------------------

    def __reduce_ex__(self, protocol):
        # CPython protocol>=3 pickles arrays through `_array_reconstructor`
        # over the raw bytes + a machine-format code (portable across
        # boxes); older protocols fall back to a list-based reduction.
        # The third element carries the instance __dict__ so subclass
        # attributes survive (test_array.test_pickle's `a.x`).
        state = getattr(self, '__dict__', None)
        if protocol >= 3:
            return (
                _array_reconstructor,
                (type(self), self._typecode,
                 _machine_format_code(self._typecode), self.tobytes()),
                state,
            )
        # Portable fallback: reconstruct via (typecode, list).
        if self._typecode in _UNICODE_TYPECODES:
            initializer = self.tounicode()
        else:
            initializer = self.tolist()
        return (type(self), (self._typecode, initializer), state)

    def __copy__(self):
        out = _make_array(self._typecode)
        out._buf = bytearray(self._buf)
        return out

    def __deepcopy__(self, memo):
        return self.__copy__()


ArrayType = array


# Machine-format codes from CPython's `arraymodule.c` `machine_format_code`
# enum: (struct format with explicit endianness, item size). Used by
# `_array_reconstructor` so a pickle made on one box reloads on another.
_MACHINE_FORMATS = {
    0:  ('<B', 1),   # UNSIGNED_INT8
    1:  ('<b', 1),   # SIGNED_INT8
    2:  ('<H', 2),   # UNSIGNED_INT16_LE
    3:  ('>H', 2),   # UNSIGNED_INT16_BE
    4:  ('<h', 2),   # SIGNED_INT16_LE
    5:  ('>h', 2),   # SIGNED_INT16_BE
    6:  ('<I', 4),   # UNSIGNED_INT32_LE
    7:  ('>I', 4),   # UNSIGNED_INT32_BE
    8:  ('<i', 4),   # SIGNED_INT32_LE
    9:  ('>i', 4),   # SIGNED_INT32_BE
    10: ('<Q', 8),   # UNSIGNED_INT64_LE
    11: ('>Q', 8),   # UNSIGNED_INT64_BE
    12: ('<q', 8),   # SIGNED_INT64_LE
    13: ('>q', 8),   # SIGNED_INT64_BE
    14: ('<f', 4),   # IEEE_754_FLOAT_LE
    15: ('>f', 4),   # IEEE_754_FLOAT_BE
    16: ('<d', 8),   # IEEE_754_DOUBLE_LE
    17: ('>d', 8),   # IEEE_754_DOUBLE_BE
    18: ('utf-16-le', 2),  # UTF16_LE
    19: ('utf-16-be', 2),  # UTF16_BE
    20: ('utf-32-le', 4),  # UTF32_LE
    21: ('utf-32-be', 4),  # UTF32_BE
}

# Per-typecode machine format on this little-endian build. 'l'/'L' and
# 'u' are platform-sized, so their codes follow the actual widths.
_TYPECODE_TO_MFC = {
    'b': 1, 'B': 0,
    'h': 4, 'H': 2,
    'i': 8, 'I': 6,
    'l': 12 if _TYPECODES['l'][1] == 8 else 8,
    'L': 10 if _TYPECODES['L'][1] == 8 else 6,
    'q': 12, 'Q': 10,
    'f': 14, 'd': 16,
    'u': 20 if _WCHAR_SIZE == 4 else 18,
    'w': 20,  # UTF32_LE (Py_UCS4, 4 bytes)
}


def _machine_format_code(typecode):
    return _TYPECODE_TO_MFC[typecode]


def _array_reconstructor(arraytype, typecode, mformat_code, items):
    """Rebuild an array pickled by `array.__reduce_ex__` (CPython parity)."""
    if not isinstance(arraytype, type):
        raise TypeError("first argument must be a type object, not %s"
                        % type(arraytype).__name__)
    if not issubclass(arraytype, array):
        raise TypeError("%r is not a subtype of array.array" % arraytype)
    if not isinstance(typecode, str) or len(typecode) != 1:
        raise TypeError("second argument must be a unicode character")
    if not isinstance(mformat_code, int) or isinstance(mformat_code, bool):
        raise TypeError("third argument must be int, not %s"
                        % type(mformat_code).__name__)
    if not isinstance(items, bytes):
        raise TypeError("fourth argument should be bytes, not %s"
                        % type(items).__name__)
    if typecode not in _TYPECODES:
        raise ValueError(
            "bad typecode (must be b, B, u, w, h, H, i, I, l, L, q, Q, f or d)"
        )
    if mformat_code not in _MACHINE_FORMATS:
        raise ValueError("third argument must be a valid machine format code.")
    a = arraytype(typecode)
    fmt, size = _MACHINE_FORMATS[mformat_code]
    if mformat_code in (18, 19, 20, 21):
        a.fromunicode(items.decode(fmt))
        return a
    if len(items) % size:
        raise ValueError("bytes length not a multiple of item size")
    is_unicode = typecode in _UNICODE_TYPECODES
    for off in range(0, len(items), size):
        v = _struct.unpack_from(fmt, items, off)[0]
        a.append(chr(v) if is_unicode else v)
    return a


# CPython's C module registers itself on import (array_modexec):
# `issubclass(array.array, collections.abc.MutableSequence)` is True.
try:
    from collections.abc import MutableSequence as _MutableSequence

    _MutableSequence.register(array)
    del _MutableSequence
except ImportError:
    pass
