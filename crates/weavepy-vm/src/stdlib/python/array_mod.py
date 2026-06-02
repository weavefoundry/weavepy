"""``array`` — efficient arrays of numeric values.

Pure-Python implementation backed by Python lists. The user-visible
surface — type codes, ``append`` / ``extend`` / ``pop`` / ``insert``,
``frombytes`` / ``tobytes``, slicing, iteration — matches CPython.
Performance for large arrays is worse than CPython's C
implementation (we don't have a typed buffer); the surface is what
ecosystem code depends on.
"""

import struct as _struct


__all__ = ['array', 'ArrayType', 'typecodes']


typecodes = 'bBuhHiIlLqQfdL'


# Map type code → (struct format, default value, expected size, doc).
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
    def __init__(self, typecode, initializer=()):
        if typecode not in _TYPECODES:
            raise ValueError('bad typecode: {!r}'.format(typecode))
        self.typecode = typecode
        self._fmt = _TYPECODES[typecode][0]
        self.itemsize = _TYPECODES[typecode][2]
        if isinstance(initializer, (bytes, bytearray)):
            self._data = []
            self.frombytes(bytes(initializer))
        else:
            self._data = []
            for item in initializer:
                self._data.append(self._coerce(item))

    def _coerce(self, item):
        if self.typecode == 'u':
            if isinstance(item, str) and len(item) == 1:
                return item
            raise TypeError('array item must be a single unicode character')
        return item

    def append(self, value):
        self._data.append(self._coerce(value))

    def extend(self, iterable):
        if isinstance(iterable, array):
            if iterable.typecode != self.typecode:
                raise TypeError('typecode mismatch')
            self._data.extend(iterable._data)
            return
        for v in iterable:
            self._data.append(self._coerce(v))

    def insert(self, index, value):
        self._data.insert(index, self._coerce(value))

    def pop(self, index=-1):
        return self._data.pop(index)

    def remove(self, value):
        self._data.remove(value)

    def reverse(self):
        self._data.reverse()

    def count(self, value):
        return self._data.count(value)

    def index(self, value, *args):
        return self._data.index(value, *args)

    def tolist(self):
        return list(self._data)

    def frombytes(self, blob):
        fmt = self._fmt
        size = self.itemsize
        if len(blob) % size:
            raise ValueError('string length not a multiple of item size')
        n = len(blob) // size
        for i in range(n):
            value, = _struct.unpack_from(fmt, blob, i * size)
            self._data.append(value)

    def tobytes(self):
        return b''.join(_struct.pack(self._fmt, v) for v in self._data)

    def __buffer__(self, flags):
        # PEP 688 buffer protocol: expose the packed bytes so buffer
        # consumers (``float``/``int``/``bytes``/``memoryview``) can read the
        # array's contents, mirroring CPython's C-level buffer export.
        return memoryview(self.tobytes())

    def fromlist(self, seq):
        for v in seq:
            self._data.append(self._coerce(v))

    def fromfile(self, fp, n):
        data = fp.read(n * self.itemsize)
        if len(data) < n * self.itemsize:
            raise EOFError
        self.frombytes(data)

    def tofile(self, fp):
        fp.write(self.tobytes())

    def fromunicode(self, s):
        if self.typecode != 'u':
            raise ValueError('not a unicode array')
        for ch in s:
            self._data.append(ch)

    def tounicode(self):
        if self.typecode != 'u':
            raise ValueError('not a unicode array')
        return ''.join(self._data)

    def buffer_info(self):
        return (id(self._data), len(self._data))

    def __len__(self):
        return len(self._data)

    def __iter__(self):
        return iter(self._data)

    def __getitem__(self, key):
        if isinstance(key, slice):
            out = array(self.typecode)
            out._data = self._data[key]
            return out
        return self._data[key]

    def __setitem__(self, key, value):
        if isinstance(key, slice):
            self._data[key] = [self._coerce(v) for v in value]
        else:
            self._data[key] = self._coerce(value)

    def __delitem__(self, key):
        del self._data[key]

    def __contains__(self, value):
        return value in self._data

    def __add__(self, other):
        if not isinstance(other, array) or other.typecode != self.typecode:
            raise TypeError('cannot add arrays of different types')
        out = array(self.typecode)
        out._data = self._data + other._data
        return out

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        out = array(self.typecode)
        out._data = self._data * n
        return out

    def __imul__(self, n):
        self._data = self._data * n
        return self

    def __repr__(self):
        if not self._data:
            return "array('{}')".format(self.typecode)
        return "array('{}', {!r})".format(self.typecode, self._data)

    def __eq__(self, other):
        if isinstance(other, array):
            return self.typecode == other.typecode and self._data == other._data
        return NotImplemented

    def __ne__(self, other):
        return not (self == other)


ArrayType = array
