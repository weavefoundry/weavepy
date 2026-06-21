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


__all__ = ['array', 'ArrayType', 'typecodes', '_array_reconstructor']


typecodes = 'bBuhHiIlLqQfd'


# Map type code -> (struct format, default value, item size, doc).
_TYPECODES = {
    'b': ('b', 0, 1, 'signed char'),
    'B': ('B', 0, 1, 'unsigned char'),
    'u': ('H', '\u0000', 2, 'unicode char'),
    'h': ('h', 0, 2, 'signed short'),
    'H': ('H', 0, 2, 'unsigned short'),
    'i': ('i', 0, 4, 'signed int'),
    'I': ('I', 0, 4, 'unsigned int'),
    'l': ('l', 0, 4, 'signed long'),
    'L': ('L', 0, 4, 'unsigned long'),
    'q': ('q', 0, 8, 'signed long long'),
    'Q': ('Q', 0, 8, 'unsigned long long'),
    'f': ('f', 0.0, 4, 'float'),
    'd': ('d', 0.0, 8, 'double'),
}


class array:
    def __init__(self, typecode, initializer=None):
        if not isinstance(typecode, str) or typecode not in _TYPECODES:
            raise ValueError(
                "bad typecode (must be b, B, u, h, H, i, I, l, L, q, Q, f or d)"
            )
        self.typecode = typecode
        self._fmt = _TYPECODES[typecode][0]
        self.itemsize = _TYPECODES[typecode][2]
        self._buf = bytearray()
        if initializer is None:
            return
        if isinstance(initializer, str):
            if typecode == 'u':
                self.fromunicode(initializer)
            else:
                raise TypeError(
                    "cannot use a str to initialize an array with typecode '%s'"
                    % typecode
                )
        elif isinstance(initializer, (bytes, bytearray)):
            self.frombytes(bytes(initializer))
        elif isinstance(initializer, array):
            if initializer.typecode == typecode:
                self._buf[:] = initializer._buf
            else:
                for v in initializer:
                    self.append(v)
        else:
            for item in initializer:
                self.append(item)

    # -- internal pack/unpack helpers ------------------------------------

    def _coerce(self, value):
        if self.typecode == 'u':
            if isinstance(value, str) and len(value) == 1:
                return value
            raise TypeError('array item must be a unicode character')
        return value

    def _pack(self, value):
        if self.typecode == 'u':
            value = self._coerce(value)
            return _struct.pack(self._fmt, ord(value))
        return _struct.pack(self._fmt, value)

    def _unpack(self, index):
        off = index * self.itemsize
        value = _struct.unpack_from(self._fmt, self._buf, off)[0]
        if self.typecode == 'u':
            return chr(value)
        return value

    def _normalize_index(self, index):
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
            if iterable.typecode != self.typecode:
                raise TypeError(
                    "can only extend with array of same kind"
                )
            self._buf += iterable._buf
            return
        for v in iterable:
            self.append(v)

    def insert(self, index, value):
        n = len(self)
        if index < 0:
            index += n
            if index < 0:
                index = 0
        elif index > n:
            index = n
        off = index * self.itemsize
        self._buf[off:off] = self._pack(value)

    def pop(self, index=-1):
        if len(self) == 0:
            raise IndexError('pop from empty array')
        index = self._normalize_index(index)
        value = self._unpack(index)
        off = index * self.itemsize
        del self._buf[off:off + self.itemsize]
        return value

    def remove(self, value):
        idx = self.index(value)
        off = idx * self.itemsize
        del self._buf[off:off + self.itemsize]

    def reverse(self):
        n = len(self)
        size = self.itemsize
        items = [bytes(self._buf[i * size:(i + 1) * size]) for i in range(n)]
        items.reverse()
        self._buf[:] = b''.join(items)

    # -- non-mutating queries -------------------------------------------

    def count(self, value):
        return sum(1 for v in self if v == value)

    def index(self, value, *args):
        # Support optional start/stop like list.index.
        n = len(self)
        start = 0
        stop = n
        if args:
            start = args[0]
            if start < 0:
                start = max(n + start, 0)
            if len(args) > 1:
                stop = args[1]
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
        return (id(self._buf), len(self))

    # -- bytes / file / unicode conversions -----------------------------

    def frombytes(self, blob):
        if isinstance(blob, str):
            raise TypeError('a bytes-like object is required, not \'str\'')
        blob = bytes(blob)
        if len(blob) % self.itemsize:
            raise ValueError('bytes length not a multiple of item size')
        self._buf += blob

    fromstring = frombytes

    def tobytes(self):
        return bytes(self._buf)

    tostring = tobytes

    def fromlist(self, seq):
        if not isinstance(seq, list):
            raise TypeError('arg must be list')
        # All-or-nothing: pack into a scratch buffer first.
        scratch = bytearray()
        for v in seq:
            scratch += self._pack(v)
        self._buf += scratch

    def fromfile(self, fp, n):
        need = n * self.itemsize
        data = fp.read(need)
        if len(data) < need:
            self.frombytes(data)
            raise EOFError("read() didn't return enough bytes")
        self.frombytes(data)

    def tofile(self, fp):
        fp.write(self.tobytes())

    def fromunicode(self, s):
        if self.typecode != 'u':
            raise ValueError("fromunicode() may only be called on "
                             "unicode type arrays")
        for ch in s:
            self.append(ch)

    def tounicode(self):
        if self.typecode != 'u':
            raise ValueError("tounicode() may only be called on "
                             "unicode type arrays")
        return ''.join(self._unpack(i) for i in range(len(self)))

    # -- PEP 688 buffer protocol ----------------------------------------

    def __buffer__(self, flags):
        # Expose the live storage so consumers read/write *through* to the
        # array (CPython's C-level buffer export). ``self._buf`` is the
        # array's own bytearray, so ``memoryview(self._buf)`` shares it.
        return memoryview(self._buf)

    # -- container protocol ---------------------------------------------

    def __len__(self):
        return len(self._buf) // self.itemsize

    def __iter__(self):
        for i in range(len(self)):
            yield self._unpack(i)

    def __getitem__(self, key):
        if isinstance(key, slice):
            out = array(self.typecode)
            indices = range(*key.indices(len(self)))
            size = self.itemsize
            chunks = []
            for i in indices:
                chunks.append(bytes(self._buf[i * size:(i + 1) * size]))
            out._buf = bytearray(b''.join(chunks))
            return out
        return self._unpack(self._normalize_index(key))

    def __setitem__(self, key, value):
        size = self.itemsize
        if isinstance(key, slice):
            start, stop, step = key.indices(len(self))
            indices = list(range(start, stop, step))
            if isinstance(value, array):
                if value.typecode != self.typecode:
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
        index = self._normalize_index(key)
        self._buf[index * size:(index + 1) * size] = self._pack(value)

    def __delitem__(self, key):
        size = self.itemsize
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
        if not isinstance(other, array) or other.typecode != self.typecode:
            raise TypeError("can only append array (not \"%s\") to array"
                            % type(other).__name__)
        out = array(self.typecode)
        out._buf = bytearray(self._buf) + other._buf
        return out

    def __iadd__(self, other):
        if not isinstance(other, array) or other.typecode != self.typecode:
            raise TypeError("can only extend array with array of same kind")
        self._buf += other._buf
        return self

    def __mul__(self, n):
        out = array(self.typecode)
        out._buf = bytearray(self._buf) * max(int(n), 0)
        return out

    __rmul__ = __mul__

    def __imul__(self, n):
        self._buf *= max(int(n), 0)
        return self

    def __repr__(self):
        if not len(self):
            return "array('%s')" % self.typecode
        if self.typecode == 'u':
            return "array('u', %r)" % self.tounicode()
        return "array('%s', %r)" % (self.typecode, self.tolist())

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
        # over the raw bytes + a machine-format code (portable across boxes);
        # older protocols fall back to a list-based reduction.
        try:
            from copyreg import __newobj__  # noqa: F401
        except ImportError:
            pass
        if protocol >= 3:
            return (
                _array_reconstructor,
                (type(self), self.typecode, _machine_format_code(self.typecode),
                 self.tobytes()),
            )
        # Portable fallback: reconstruct via (typecode, list).
        if self.typecode == 'u':
            initializer = self.tounicode()
        else:
            initializer = self.tolist()
        return (type(self), (self.typecode, initializer))

    def __copy__(self):
        out = array(self.typecode)
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

# Per-typecode machine format on this (little-endian, standard-size) build.
_TYPECODE_TO_MFC = {
    'b': 1, 'B': 0,
    'h': 4, 'H': 2,
    'i': 8, 'I': 6,
    'l': 8, 'L': 6,
    'q': 12, 'Q': 10,
    'f': 14, 'd': 16,
    'u': 18,
}


def _machine_format_code(typecode):
    return _TYPECODE_TO_MFC[typecode]


def _array_reconstructor(arraytype, typecode, mformat_code, items):
    """Rebuild an array pickled by `array.__reduce_ex__` (CPython parity)."""
    if not isinstance(arraytype, type) or not issubclass(arraytype, array):
        raise TypeError("first argument must be a type object")
    if not isinstance(items, bytes):
        raise TypeError("fourth argument should be bytes, not %s"
                        % type(items).__name__)
    if mformat_code not in _MACHINE_FORMATS:
        raise ValueError("third argument must be a valid machine format code.")
    a = arraytype(typecode)
    fmt, size = _MACHINE_FORMATS[mformat_code]
    if mformat_code in (18, 19, 20, 21):
        a.fromunicode(items.decode(fmt))
        return a
    if len(items) % size:
        raise ValueError("bytes length not a multiple of item size")
    for off in range(0, len(items), size):
        a.append(_struct.unpack_from(fmt, items, off)[0])
    return a


# CPython's C module registers itself on import (array_modexec):
# `issubclass(array.array, collections.abc.MutableSequence)` is True.
try:
    from collections.abc import MutableSequence as _MutableSequence

    _MutableSequence.register(array)
    del _MutableSequence
except ImportError:
    pass
