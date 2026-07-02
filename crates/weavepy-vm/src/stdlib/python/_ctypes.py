"""WeavePy reimplementation of the CPython ``_ctypes`` extension module.

CPython ships ``_ctypes`` as a *core-built* C extension: it links against
private interpreter internals (``_PyRuntime`` & friends), so unlike a
stable-ABI wheel (numpy, pandas) its compiled ``_ctypes*.so`` cannot be
``dlopen``'d into WeavePy. We therefore reimplement the exact public
surface that CPython's verbatim ``Lib/ctypes/__init__.py`` imports, layered
on the native :mod:`_ctypes_native` primitive module.

The split mirrors CPython's own ``Lib/ctypes`` (Python) over ``_ctypes``
(C):

* :mod:`_ctypes_native` (Rust) owns the genuinely-native pieces — platform C
  type sizes/alignments, raw memory peek/poke, ``dlopen``/``dlsym``, the
  libc ``memmove``/``memset``/``string_at`` helpers, the ctypes private
  errno, and the libffi call/closure bridge.
* This module builds the ``_SimpleCData`` / ``Structure`` / ``Union`` /
  ``Array`` / ``_Pointer`` / ``CFuncPtr`` type system and its metaclasses
  on top of those primitives.

Memory model
------------
A ctypes object's storage is a Python ``bytearray`` (owned, GC'd, and
address-stable while its length is fixed — ctypes objects never resize
except via :func:`resize`). Views — ``Structure`` fields, ``Array``
elements, :meth:`from_buffer` — share that ``bytearray`` at an offset.
External memory (:meth:`from_address`, pointer dereference, FFI return
pointers) is addressed by a raw integer. Every object therefore resolves to
a single ``void *`` via :func:`addressof`, exactly like CPython's
``CDataObject.b_ptr``.
"""

import sys as _sys
import _ctypes_native as _nat

__version__ = "1.1.0"

# ---------------------------------------------------------------------------
# Platform constants (re-exported by ctypes/__init__.py)
# ---------------------------------------------------------------------------

RTLD_LOCAL = _nat.RTLD_LOCAL
RTLD_GLOBAL = _nat.RTLD_GLOBAL
SIZEOF_TIME_T = _nat.SIZEOF_TIME_T

_PTR = _nat.SIZEOF_VOID_P
_WCHAR = _nat.sizeof_code("u")
_BO = _sys.byteorder
_FP = "<" if _BO == "little" else ">"

# Function-pointer calling-convention / behaviour flags. Values match
# CPython's ``Modules/_ctypes/ctypes.h`` so ctypes/__init__.py's bit math is
# faithful.
FUNCFLAG_CDECL = 0x1
FUNCFLAG_HRESULT = 0x2
FUNCFLAG_PYTHONAPI = 0x4
FUNCFLAG_USE_ERRNO = 0x8
FUNCFLAG_USE_LASTERROR = 0x10
# stdcall is Windows-only; defined so the constant exists everywhere.
FUNCFLAG_STDCALL = 0x0

# Type-flag bits used by from_param's "PARAMFLAG" plumbing.
TYPEFLAG_ISPOINTER = 0x100
TYPEFLAG_HASPOINTER = 0x200


class ArgumentError(Exception):
    """Raised when a foreign function call gets an argument it can't
    convert (CPython exposes this from ``_ctypes``)."""


def get_errno():
    return _nat.get_errno()


def set_errno(value):
    return _nat.set_errno(value)


# ---------------------------------------------------------------------------
# Low-level value codecs for the simple ``_type_`` format codes
# ---------------------------------------------------------------------------

# Integer codes -> (size, signed).
_INT_CODES = {
    "b": (1, True),
    "B": (1, False),
    "h": (_nat.sizeof_code("h"), True),
    "H": (_nat.sizeof_code("H"), False),
    "i": (_nat.sizeof_code("i"), True),
    "I": (_nat.sizeof_code("i"), False),
    "l": (_nat.sizeof_code("l"), True),
    "L": (_nat.sizeof_code("l"), False),
    "q": (_nat.sizeof_code("q"), True),
    "Q": (_nat.sizeof_code("q"), False),
}


def _read_at(obj, off, n):
    """Read ``n`` bytes from ``obj``'s memory at relative offset ``off``."""
    buf = obj._b_buffer
    if buf is not None:
        start = obj._b_offset + off
        return bytes(buf[start:start + n])
    return _nat.read_mem(obj._b_addr + off, n)


def _write_at(obj, off, data):
    buf = obj._b_buffer
    if buf is not None:
        start = obj._b_offset + off
        buf[start:start + len(data)] = data
    else:
        _nat.write_mem(obj._b_addr + off, data)


def _simple_get(code, obj, off=0):
    if code in _INT_CODES:
        size, signed = _INT_CODES[code]
        v = int.from_bytes(_read_at(obj, off, size), _BO)
        # Apply two's-complement sign manually: CPython makes ``signed`` a
        # keyword-only arg, which not every host int.from_bytes honours.
        if signed and v >= (1 << (size * 8 - 1)):
            v -= 1 << (size * 8)
        return v
    if code == "f":
        import struct as _struct
        return _struct.unpack(_FP + "f", _read_at(obj, off, 4))[0]
    if code in ("d", "g"):
        import struct as _struct
        sz = _nat.sizeof_code(code)
        if sz == 8:
            return _struct.unpack(_FP + "d", _read_at(obj, off, 8))[0]
        # 80-bit / 128-bit long double: not yet decoded to a Python float.
        raise NotImplementedError("long double (>8 bytes) not supported yet")
    if code == "c":
        return _read_at(obj, off, 1)
    if code == "?":
        return _read_at(obj, off, 1)[0] != 0
    if code == "u":
        cp = int.from_bytes(_read_at(obj, off, _WCHAR), _BO)
        return chr(cp)
    if code == "P":
        v = int.from_bytes(_read_at(obj, off, _PTR), _BO)
        return v if v else None
    if code == "z":
        v = int.from_bytes(_read_at(obj, off, _PTR), _BO)
        return _nat.string_at(v, -1) if v else None
    if code == "Z":
        v = int.from_bytes(_read_at(obj, off, _PTR), _BO)
        return _nat.wstring_at(v, -1) if v else None
    if code == "O":
        # py_object: the live Python object is held on the keepalive list;
        # the buffer stores its id() purely as a presence marker.
        v = int.from_bytes(_read_at(obj, off, _PTR), _BO)
        if not v:
            raise ValueError("PyObject is NULL")
        ka = obj._b_objects
        if ka:
            for kept in ka:
                if id(kept) == v:
                    return kept
        raise ValueError("PyObject is NULL")
    raise TypeError("unknown type code %r" % code)


def _simple_set(code, obj, value, off=0):
    if code in _INT_CODES:
        size, signed = _INT_CODES[code]
        iv = int(value) & ((1 << (size * 8)) - 1)
        _write_at(obj, off, iv.to_bytes(size, _BO))
        return
    if code == "f":
        import struct as _struct
        _write_at(obj, off, _struct.pack(_FP + "f", float(value)))
        return
    if code in ("d", "g"):
        import struct as _struct
        sz = _nat.sizeof_code(code)
        if sz == 8:
            _write_at(obj, off, _struct.pack(_FP + "d", float(value)))
            return
        raise NotImplementedError("long double (>8 bytes) not supported yet")
    if code == "c":
        if isinstance(value, int):
            b = bytes([value & 0xFF])
        elif isinstance(value, (bytes, bytearray)) and len(value) == 1:
            b = bytes(value)
        else:
            raise TypeError("one character bytes, bytearray or integer expected")
        _write_at(obj, off, b)
        return
    if code == "?":
        _write_at(obj, off, b"\x01" if value else b"\x00")
        return
    if code == "u":
        if not isinstance(value, str) or len(value) != 1:
            raise TypeError("unicode string expected instead of %s instance"
                            % type(value).__name__)
        _write_at(obj, off, ord(value).to_bytes(_WCHAR, _BO))
        return
    if code == "P":
        iv = _as_address(value)
        if isinstance(value, _CData):
            obj._keep(value)
        _write_at(obj, off, iv.to_bytes(_PTR, _BO))
        return
    if code == "z":
        if value is None:
            iv = 0
        elif isinstance(value, int):
            iv = value
        elif isinstance(value, (bytes, bytearray)):
            kb = bytearray(value)
            kb.append(0)  # NUL-terminate
            obj._keep(kb)
            iv = _nat.addressof_buffer(kb)
        elif isinstance(value, _CData):
            obj._keep(value)
            iv = addressof(value)
        else:
            raise TypeError("bytes or integer address expected instead of %s instance"
                            % type(value).__name__)
        _write_at(obj, off, iv.to_bytes(_PTR, _BO))
        return
    if code == "Z":
        if value is None:
            iv = 0
        elif isinstance(value, int):
            iv = value
        elif isinstance(value, str):
            kb = bytearray()
            for ch in value:
                kb += ord(ch).to_bytes(_WCHAR, _BO)
            kb += (0).to_bytes(_WCHAR, _BO)
            obj._keep(kb)
            iv = _nat.addressof_buffer(kb)
        else:
            raise TypeError("unicode string or integer address expected")
        _write_at(obj, off, iv.to_bytes(_PTR, _BO))
        return
    if code == "O":
        obj._keep(value)
        _write_at(obj, off, id(value).to_bytes(_PTR, _BO))
        return
    raise TypeError("unknown type code %r" % code)


def _as_address(x):
    """Coerce a Python value to an integer machine address."""
    if x is None:
        return 0
    if isinstance(x, bool):
        return int(x)
    if isinstance(x, int):
        return x
    if isinstance(x, _CArgObject):
        return x._address()
    if isinstance(x, _CData):
        return addressof(x)
    raise TypeError("cannot convert %r to an address" % (type(x).__name__,))


# ---------------------------------------------------------------------------
# Metaclasses
# ---------------------------------------------------------------------------


class _CDataType(type):
    """Common metaclass behaviour shared by every ctypes type.

    Methods defined here are ``CDataType`` methods in CPython: they live on
    the metaclass, so they are callable on the *class* object (e.g.
    ``c_int.from_address(...)``, ``Point * 4``).
    """

    def __mul__(cls, length):
        return _create_array_type(cls, length)

    def __rmul__(cls, length):
        return _create_array_type(cls, length)

    # -- construction from existing memory -------------------------------

    def from_address(cls, address):
        inst = _blank(cls)
        inst._b_buffer = None
        inst._b_offset = 0
        inst._b_addr = int(address)
        inst._b_base = None
        inst._b_objects = None
        return inst

    def from_buffer(cls, source, offset=0):
        ba = _writable_buffer(source)
        size = sizeof(cls)
        if offset < 0:
            raise ValueError("offset cannot be negative")
        if len(ba) - offset < size:
            raise ValueError(
                "Buffer size too small (%d instead of at least %d bytes)"
                % (len(ba), size + offset)
            )
        inst = _blank(cls)
        inst._b_buffer = ba
        inst._b_offset = offset
        inst._b_base = source
        inst._b_objects = None
        return inst

    def from_buffer_copy(cls, source, offset=0):
        size = sizeof(cls)
        data = bytes(source)
        if offset < 0:
            raise ValueError("offset cannot be negative")
        if len(data) - offset < size:
            raise ValueError(
                "Buffer size too small (%d instead of at least %d bytes)"
                % (len(data), size + offset)
            )
        inst = _alloc_instance(cls)
        _write_at(inst, 0, data[offset:offset + size])
        return inst

    def in_dll(cls, dll, name):
        addr = _nat.dlsym(dll._handle, name)
        if not addr:
            raise ValueError("symbol %r not found" % name)
        return cls.from_address(addr)

    def from_param(cls, value):
        return _default_from_param(cls, value)

    def __call__(cls, *args, **kw):
        # Identical to type.__call__, but kept explicit so the data-model
        # `__new__`/`__init__` flow is unambiguous for the C-style types.
        return type.__call__(cls, *args, **kw)


class _SimpleType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        code = namespace.get("_type_", None)
        if code is None:
            code = getattr(cls, "_type_", None)
        if code is not None:
            if not isinstance(code, str) or len(code) != 1:
                raise ValueError(
                    "class must define a '_type_' string attribute of length 1"
                )
            cls._b_size_ = _nat.sizeof_code(code)
            cls._b_align_ = _nat.alignment_code(code)
        else:
            cls._b_size_ = 0
            cls._b_align_ = 1


class _StructType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        _init_aggregate(cls, namespace, union=False)

    def __setattr__(cls, key, value):
        if key == "_fields_":
            type.__setattr__(cls, key, value)
            _layout_aggregate(cls, value, union=False)
        else:
            type.__setattr__(cls, key, value)


class _UnionType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        _init_aggregate(cls, namespace, union=True)

    def __setattr__(cls, key, value):
        if key == "_fields_":
            type.__setattr__(cls, key, value)
            _layout_aggregate(cls, value, union=True)
        else:
            type.__setattr__(cls, key, value)


class _ArrayType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        etype = getattr(cls, "_type_", None)
        length = getattr(cls, "_length_", None)
        if etype is not None and length is not None:
            cls._b_size_ = sizeof(etype) * length
            cls._b_align_ = alignment(etype)
        else:
            cls._b_size_ = 0
            cls._b_align_ = 1


class _PointerType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        cls._b_size_ = _PTR
        cls._b_align_ = _PTR

    def set_type(cls, t):
        cls._type_ = t


class _FuncPtrType(_CDataType):
    def __init__(cls, name, bases, namespace, **kw):
        super().__init__(name, bases, namespace)
        cls._b_size_ = _PTR
        cls._b_align_ = _PTR


# ---------------------------------------------------------------------------
# Instance allocation helpers
# ---------------------------------------------------------------------------


def _blank(cls):
    """A bare instance of ``cls`` with no memory set up yet (callers fill in
    the ``_b_*`` slots). Bypasses ``_CData.__new__`` allocation."""
    return object.__new__(cls)


def _alloc_instance(cls):
    """An instance of ``cls`` backed by fresh zeroed owned memory."""
    inst = object.__new__(cls)
    inst._b_buffer = bytearray(sizeof(cls))
    inst._b_offset = 0
    inst._b_addr = 0
    inst._b_base = None
    inst._b_objects = None
    return inst


def _field_view(parent, ftype, field_offset):
    """A sub-object of ``ftype`` aliasing ``parent``'s memory at an offset."""
    inst = _blank(ftype)
    if parent._b_buffer is not None:
        inst._b_buffer = parent._b_buffer
        inst._b_offset = parent._b_offset + field_offset
        inst._b_addr = 0
    else:
        inst._b_buffer = None
        inst._b_offset = 0
        inst._b_addr = parent._b_addr + field_offset
    inst._b_base = parent
    inst._b_objects = None
    return inst


# ---------------------------------------------------------------------------
# Base data classes
# ---------------------------------------------------------------------------


class _CData(metaclass=_CDataType):
    _b_size_ = 0
    _b_align_ = 1

    def __new__(cls, *args, **kw):
        return _alloc_instance(cls)

    def __init__(self, *args, **kw):
        pass

    # -- memory helpers --------------------------------------------------

    def _addr(self):
        if self._b_buffer is not None:
            return _nat.addressof_buffer(self._b_buffer) + self._b_offset
        return self._b_addr

    def _keep(self, obj):
        if self._b_objects is None:
            self._b_objects = []
        self._b_objects.append(obj)

    def _read(self, off, n):
        return _read_at(self, off, n)

    def _write(self, off, data):
        _write_at(self, off, data)

    def __ctypes_from_outparam__(self):
        return self


class _SimpleCData(_CData, metaclass=_SimpleType):
    _type_ = None

    def __init__(self, value=None):
        if value is not None:
            self.value = value

    @property
    def value(self):
        return _simple_get(type(self)._type_, self)

    @value.setter
    def value(self, v):
        _simple_set(type(self)._type_, self, v)

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self.value)

    def __bool__(self):
        return any(self._read(0, sizeof(type(self))))

    def __eq__(self, other):
        if isinstance(other, _SimpleCData):
            return self.value == other.value
        return self.value == other

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return hash(self.value)


# -- Structure / Union -------------------------------------------------------


class _Field:
    __slots__ = ("name", "ftype", "offset", "size", "bits", "bit_offset")

    def __init__(self, name, ftype, offset, size, bits=None, bit_offset=0):
        self.name = name
        self.ftype = ftype
        self.offset = offset
        self.size = size
        self.bits = bits
        self.bit_offset = bit_offset


def _round_up(n, align):
    if align <= 1:
        return n
    rem = n % align
    return n if rem == 0 else n + (align - rem)


def _init_aggregate(cls, namespace, union):
    # Inherit a base aggregate's already-computed layout, then (if this
    # class body supplied `_fields_`) extend it.
    base_layout = None
    for base in cls.__mro__[1:]:
        bl = base.__dict__.get("_b_layout_")
        if bl is not None:
            base_layout = bl
            break
    if base_layout is not None and "_b_layout_" not in cls.__dict__:
        cls._b_layout_ = dict(base_layout)
        cls._b_size_ = base.__dict__.get("_b_size_", 0)
        cls._b_align_ = base.__dict__.get("_b_align_", 1)
    else:
        cls._b_layout_ = {}
        cls._b_size_ = 0
        cls._b_align_ = 1
    if "_fields_" in namespace:
        _layout_aggregate(cls, namespace["_fields_"], union)


def _layout_aggregate(cls, fields, union):
    layout = {}
    # Start after any inherited base fields (structs append; unions overlay).
    base_size = 0
    base_align = 1
    for base in cls.__mro__[1:]:
        bs = base.__dict__.get("_b_size_")
        if bs:
            base_size = bs
            base_align = base.__dict__.get("_b_align_", 1)
            layout.update(base.__dict__.get("_b_layout_", {}))
            break

    pack = getattr(cls, "_pack_", 0)
    offset = 0 if union else base_size
    total_align = base_align
    max_size = base_size

    for item in fields:
        fname = item[0]
        ftype = item[1]
        bits = item[2] if len(item) > 2 else None
        if not isinstance(ftype, _CDataType):
            raise TypeError("second item in _fields_ tuple (index 0) must be a C type")
        fsize = sizeof(ftype)
        falign = alignment(ftype)
        if pack:
            falign = min(falign, pack)
        if union:
            foffset = base_size  # all union members overlay at the base offset
            max_size = max(max_size, fsize)
        else:
            offset = _round_up(offset, falign)
            foffset = offset
            offset += fsize
        total_align = max(total_align, falign)
        fld = _Field(fname, ftype, foffset, fsize, bits)
        layout[fname] = fld
        type.__setattr__(cls, fname, _make_field_descriptor(fld))

    if union:
        total = _round_up(max_size, total_align)
    else:
        total = _round_up(offset, total_align)

    cls._b_layout_ = layout
    type.__setattr__(cls, "_b_size_", total)
    type.__setattr__(cls, "_b_align_", total_align)


def _is_char_type(t):
    return (
        isinstance(t, _CDataType)
        and issubclass(t, _SimpleCData)
        and getattr(t, "_type_", None) == "c"
    )


def _is_wchar_type(t):
    return (
        isinstance(t, _CDataType)
        and issubclass(t, _SimpleCData)
        and getattr(t, "_type_", None) == "u"
    )


def _make_field_descriptor(fld):
    ftype = fld.ftype
    offset = fld.offset
    simple = issubclass(ftype, _SimpleCData)

    def getter(self):
        if simple:
            return _simple_get(ftype._type_, self, offset)
        return _field_view(self, ftype, offset)

    def setter(self, value):
        if simple:
            _simple_set(ftype._type_, self, value, offset)
            return
        # Aggregate / pointer / array field: copy the value's bytes in.
        if isinstance(value, _CData):
            _write_at(self, offset, value._read(0, sizeof(ftype)))
            if value._b_objects:
                for k in value._b_objects:
                    self._keep(k)
        else:
            tmp = ftype(value) if not isinstance(value, (list, tuple)) else ftype(*value)
            _write_at(self, offset, tmp._read(0, sizeof(ftype)))
            if tmp._b_objects:
                for k in tmp._b_objects:
                    self._keep(k)

    return property(getter, setter)


class Structure(_CData, metaclass=_StructType):
    def __init__(self, *args, **kw):
        layout = type(self)._b_layout_
        names = list(layout.keys())
        for i, val in enumerate(args):
            if i >= len(names):
                raise TypeError("too many initializers")
            setattr(self, names[i], val)
        for key, val in kw.items():
            if key not in layout:
                raise AttributeError(
                    "'%s' is not a valid field name" % key
                )
            setattr(self, key, val)


class Union(_CData, metaclass=_UnionType):
    def __init__(self, *args, **kw):
        layout = type(self)._b_layout_
        names = list(layout.keys())
        for i, val in enumerate(args):
            if i >= len(names):
                raise TypeError("too many initializers")
            setattr(self, names[i], val)
        for key, val in kw.items():
            if key not in layout:
                raise AttributeError("'%s' is not a valid field name" % key)
            setattr(self, key, val)


# -- Array -------------------------------------------------------------------

_array_cache = {}


def _create_array_type(element_type, length):
    if isinstance(length, bool) or not isinstance(length, int):
        raise TypeError("can't multiply a ctypes type by a non-integer")
    if length < 0:
        raise ValueError("Array length must be >= 0, not %d" % length)
    if not isinstance(element_type, _CDataType):
        raise TypeError("Expected a ctypes type")
    key = (element_type, length)
    cached = _array_cache.get(key)
    if cached is not None:
        return cached
    name = "%s_Array_%d" % (element_type.__name__, length)
    arr = _ArrayType(name, (Array,), {"_type_": element_type, "_length_": length})
    _array_cache[key] = arr
    return arr


class Array(_CData, metaclass=_ArrayType):
    def __init__(self, *args):
        if args:
            for i, val in enumerate(args):
                self[i] = val

    def __len__(self):
        return type(self)._length_

    def _check_index(self, index):
        n = type(self)._length_
        if index < 0:
            index += n
        if not (0 <= index < n):
            raise IndexError("invalid index")
        return index

    def __getitem__(self, index):
        etype = type(self)._type_
        esize = sizeof(etype)
        if isinstance(index, slice):
            return [self[i] for i in range(*index.indices(len(self)))]
        index = self._check_index(index)
        if issubclass(etype, _SimpleCData):
            return _simple_get(etype._type_, self, index * esize)
        return _field_view(self, etype, index * esize)

    def __setitem__(self, index, value):
        etype = type(self)._type_
        esize = sizeof(etype)
        if isinstance(index, slice):
            for i, v in zip(range(*index.indices(len(self))), value):
                self[i] = v
            return
        index = self._check_index(index)
        if issubclass(etype, _SimpleCData):
            _simple_set(etype._type_, self, value, index * esize)
            return
        if isinstance(value, _CData):
            _write_at(self, index * esize, value._read(0, esize))
        else:
            tmp = etype(value)
            _write_at(self, index * esize, tmp._read(0, esize))

    def __iter__(self):
        for i in range(len(self)):
            yield self[i]

    @property
    def value(self):
        etype = type(self)._type_
        if _is_char_type(etype):
            data = self._read(0, sizeof(type(self)))
            nul = data.find(b"\x00")
            return data if nul < 0 else data[:nul]
        if _is_wchar_type(etype):
            chars = []
            for i in range(len(self)):
                ch = self[i]
                if ch == "\x00":
                    break
                chars.append(ch)
            return "".join(chars)
        raise AttributeError("value")

    @value.setter
    def value(self, val):
        etype = type(self)._type_
        if _is_char_type(etype):
            if not isinstance(val, (bytes, bytearray)):
                raise TypeError("bytes expected instead of %s" % type(val).__name__)
            if len(val) > len(self):
                raise ValueError("bytes too long")
            data = bytes(val)
            _write_at(self, 0, data)
            if len(data) < len(self):
                _write_at(self, len(data), b"\x00")
            return
        if _is_wchar_type(etype):
            if not isinstance(val, str):
                raise TypeError("unicode expected")
            for i, ch in enumerate(val):
                self[i] = ch
            if len(val) < len(self):
                self[len(val)] = "\x00"
            return
        raise AttributeError("value")

    @property
    def raw(self):
        if not _is_char_type(type(self)._type_):
            raise AttributeError("raw")
        return self._read(0, sizeof(type(self)))

    @raw.setter
    def raw(self, val):
        if not _is_char_type(type(self)._type_):
            raise AttributeError("raw")
        data = bytes(val)
        if len(data) > len(self):
            raise ValueError("bytes too long")
        _write_at(self, 0, data)


# -- Pointer -----------------------------------------------------------------

_pointer_type_cache = {}


class _Pointer(_CData, metaclass=_PointerType):
    _type_ = None

    def __init__(self, value=None):
        if value is not None:
            self.contents = value

    def _target_addr(self):
        return int.from_bytes(self._read(0, _PTR), _BO)

    @property
    def contents(self):
        addr = self._target_addr()
        if addr == 0:
            raise ValueError("NULL pointer access")
        tgt = type(self)._type_
        view = tgt.from_address(addr)
        view._b_base = self
        return view

    @contents.setter
    def contents(self, value):
        if not isinstance(value, _CData):
            raise TypeError("expected a ctypes instance")
        self._write(0, addressof(value).to_bytes(_PTR, _BO))
        self._keep(value)

    def __getitem__(self, index):
        tgt = type(self)._type_
        esize = sizeof(tgt)
        base = self._target_addr()
        if base == 0:
            raise ValueError("NULL pointer access")
        if issubclass(tgt, _SimpleCData):
            tmp = tgt.from_address(base + index * esize)
            return _simple_get(tgt._type_, tmp, 0)
        view = tgt.from_address(base + index * esize)
        view._b_base = self
        return view

    def __setitem__(self, index, value):
        tgt = type(self)._type_
        esize = sizeof(tgt)
        base = self._target_addr()
        if base == 0:
            raise ValueError("NULL pointer access")
        dst = tgt.from_address(base + index * esize)
        if issubclass(tgt, _SimpleCData):
            _simple_set(tgt._type_, dst, value, 0)
        elif isinstance(value, _CData):
            _write_at(dst, 0, value._read(0, esize))
        else:
            tmp = tgt(value)
            _write_at(dst, 0, tmp._read(0, esize))

    def __bool__(self):
        return self._target_addr() != 0


def POINTER(cls):
    try:
        return _pointer_type_cache[cls]
    except KeyError:
        pass
    if cls is None:
        # CPython allows POINTER(None) -> a not-yet-typed pointer; the cache
        # is later seeded with `_pointer_type_cache[None] = c_void_p` by
        # ctypes._reset_cache().
        name = "LP_None"
        ptr = _PointerType(name, (_Pointer,), {"_type_": None})
        _pointer_type_cache[cls] = ptr
        return ptr
    name = "LP_%s" % cls.__name__
    ptr = _PointerType(name, (_Pointer,), {"_type_": cls})
    _pointer_type_cache[cls] = ptr
    return ptr


def pointer(obj):
    if not isinstance(obj, _CData):
        raise TypeError("_type_ must have storage info")
    ptr_type = POINTER(type(obj))
    p = ptr_type()
    p.contents = obj
    return p


# -- CFuncPtr ----------------------------------------------------------------


class CFuncPtr(_CData, metaclass=_FuncPtrType):
    _argtypes_ = None
    _restype_ = None
    _flags_ = FUNCFLAG_CDECL
    _closure = None
    _errcheck_ = None

    # `restype`/`argtypes`/`errcheck` are per-instance configuration on a
    # foreign function (e.g. `libc.strlen.restype = c_size_t`). They shadow
    # the class defaults (`_restype_` defaults to `c_int` on a CDLL's
    # `_FuncPtr`; `_argtypes_` defaults to `None`), so normal attribute
    # resolution in `__call__` picks the instance value when set and falls
    # back to the class otherwise. This mirrors CPython's getset descriptors
    # over the C-level slots.
    @property
    def restype(self):
        return self._restype_

    @restype.setter
    def restype(self, value):
        self._restype_ = value

    @restype.deleter
    def restype(self):
        try:
            del self.__dict__["_restype_"]
        except KeyError:
            pass

    @property
    def argtypes(self):
        return self._argtypes_

    @argtypes.setter
    def argtypes(self, value):
        self._argtypes_ = None if value is None else tuple(value)

    @argtypes.deleter
    def argtypes(self):
        try:
            del self.__dict__["_argtypes_"]
        except KeyError:
            pass

    @property
    def errcheck(self):
        return self._errcheck_

    @errcheck.setter
    def errcheck(self, value):
        if value is not None and not callable(value):
            raise TypeError("the errcheck attribute must be callable")
        self._errcheck_ = value

    def __init__(self, arg=0, *extra):
        self._handle_addr = 0
        self._callable = None
        self._name = None
        if isinstance(arg, int):
            self._set_address(arg)
        elif isinstance(arg, tuple):
            name_or_ord, dll = arg
            addr = _resolve_dll_symbol(dll, name_or_ord)
            self._set_address(addr)
            if not isinstance(name_or_ord, int):
                self._name = name_or_ord
        elif callable(arg):
            self._callable = arg
            closure_addr = _make_closure(self, arg)
            self._set_address(closure_addr)
        else:
            raise TypeError(
                "argument must be callable or integer function address"
            )

    def _set_address(self, addr):
        self._handle_addr = int(addr)
        self._write(0, int(addr).to_bytes(_PTR, _BO))

    def __call__(self, *args):
        handle = self._handle_addr
        thunk = _INTERNAL_THUNKS.get(handle)
        if thunk is not None:
            return thunk(*args)
        if self._callable is not None and handle == 0:
            return self._callable(*args)
        # Instance attributes (set via the `restype`/`argtypes` descriptors)
        # shadow the class defaults; plain attribute lookup gives instance
        # value when set, else the class default (`_restype_` -> `c_int`,
        # `_argtypes_` -> `None`).
        argtypes = self._argtypes_
        restype = self._restype_
        flags = type(self)._flags_
        result = _ffi_invoke(handle, restype, argtypes, flags, args)
        errcheck = self._errcheck_
        if errcheck is not None:
            result = errcheck(result, self, args)
        return result


# ---------------------------------------------------------------------------
# Public helpers
# ---------------------------------------------------------------------------


class _CArgObject:
    """Lightweight pass-by-reference wrapper produced by :func:`byref`."""

    __slots__ = ("_obj", "_offset")

    def __init__(self, obj, offset):
        self._obj = obj
        self._offset = offset

    def _address(self):
        return addressof(self._obj) + self._offset

    def __repr__(self):
        return "<cparam '%s' (%#x)>" % ("P", self._address())


def byref(obj, offset=0):
    if not isinstance(obj, _CData):
        raise TypeError("byref() argument must be a ctypes instance, not '%s'"
                        % type(obj).__name__)
    return _CArgObject(obj, offset)


def sizeof(type_or_obj):
    if isinstance(type_or_obj, _CDataType):
        return type_or_obj._b_size_
    if isinstance(type_or_obj, _CData):
        return type(type_or_obj)._b_size_
    raise TypeError("this type has no size")


def alignment(type_or_obj):
    if isinstance(type_or_obj, _CDataType):
        return type_or_obj._b_align_
    if isinstance(type_or_obj, _CData):
        return type(type_or_obj)._b_align_
    raise TypeError("no alignment info")


def addressof(obj):
    if not isinstance(obj, _CData):
        raise TypeError("invalid type")
    return obj._addr()


def resize(obj, size):
    if not isinstance(obj, _CData):
        raise TypeError("expected ctypes instance")
    min_size = type(obj)._b_size_
    if size < min_size:
        raise ValueError("minimum size is %d" % min_size)
    if obj._b_buffer is None:
        raise ValueError("Memory cannot be resized because this object doesn't own it")
    cur = obj._b_buffer
    if size > len(cur):
        cur.extend(b"\x00" * (size - len(cur)))


def _default_from_param(cls, value):
    if value is None:
        return None
    if isinstance(value, cls):
        return value
    if isinstance(value, _CArgObject):
        return value
    # Try to construct; mirrors CPython's "exact type or convertible".
    try:
        return cls(value)
    except (TypeError, ValueError):
        raise TypeError(
            "expected %s instance instead of %s"
            % (cls.__name__, type(value).__name__)
        )


def _writable_buffer(source):
    if isinstance(source, bytearray):
        return source
    if isinstance(source, _CData):
        # Share the owning bytearray if there is one.
        if source._b_buffer is not None:
            return source._b_buffer
    if isinstance(source, memoryview) and not source.readonly:
        return bytearray(source)  # NOTE: copy; true zero-copy needs buffer API
    raise TypeError("underlying buffer is not writable")


def _resolve_dll_symbol(dll, name_or_ord):
    if isinstance(name_or_ord, int):
        raise TypeError("ordinal lookup is only supported on Windows")
    handle = dll._handle
    addr = _nat.dlsym(handle, name_or_ord)
    if not addr:
        raise AttributeError(
            "function %r not found" % (name_or_ord,)
        )
    return addr


# ---------------------------------------------------------------------------
# dlopen (posix) — re-exported by ctypes/__init__.py as `_dlopen`
# ---------------------------------------------------------------------------


def dlopen(name, mode=RTLD_LOCAL):
    return _nat.dlopen(name, mode)


def dlclose(handle):
    return _nat.dlclose(handle)


def dlsym(handle, name):
    return _nat.dlsym(handle, name)


# ---------------------------------------------------------------------------
# Internal thunks for the addr-wrapped helpers ctypes/__init__.py builds
# (`memmove`, `memset`, `cast`, `string_at`, `wstring_at`). CPython exposes
# these as C function addresses and ctypes wraps them in CFUNCTYPE; we route
# the sentinel "addresses" back to native/Python implementations because two
# of them (cast / string_at) have PyObject semantics that can't be a plain C
# call. They are only ever *invoked* at runtime, never at import.
# ---------------------------------------------------------------------------

_INTERNAL_THUNKS = {}
_next_thunk_id = 1


def _register_thunk(fn):
    global _next_thunk_id
    addr = _next_thunk_id
    _next_thunk_id += 1
    _INTERNAL_THUNKS[addr] = fn
    return addr


def _thunk_memmove(dst, src, count):
    return _nat.memmove(_as_address(dst), _as_address(src), int(count))


def _thunk_memset(dst, c, count):
    return _nat.memset(_as_address(dst), int(c), int(count))


def _thunk_string_at(ptr, size=-1):
    return _nat.string_at(_as_address(ptr), int(size))


def _thunk_wstring_at(ptr, size=-1):
    return _nat.wstring_at(_as_address(ptr), int(size))


def _thunk_cast(ptr, obj, typ):
    # cast(obj, typ): reinterpret obj's memory as `typ`, keeping obj alive.
    if isinstance(obj, _CData):
        addr = addressof(obj)
    else:
        addr = _as_address(ptr)
    result = typ.from_address(addr)
    result._b_base = obj
    return result


_memmove_addr = _register_thunk(_thunk_memmove)
_memset_addr = _register_thunk(_thunk_memset)
_string_at_addr = _register_thunk(_thunk_string_at)
_wstring_at_addr = _register_thunk(_thunk_wstring_at)
_cast_addr = _register_thunk(_thunk_cast)


# ---------------------------------------------------------------------------
# Foreign function invocation + callbacks (libffi bridge)
# ---------------------------------------------------------------------------


def _type_code_for_ffi(t):
    """Map a ctypes type (or None) to the format code the native libffi
    bridge understands."""
    if t is None:
        return None  # void
    if isinstance(t, _CDataType):
        if issubclass(t, _SimpleCData):
            return t._type_
        if issubclass(t, (_Pointer, Array, Structure, Union)):
            return "P"
        if issubclass(t, CFuncPtr):
            return "P"
    raise TypeError("unsupported ctypes type in FFI signature: %r" % (t,))


def _arg_to_ffi(value):
    """Marshal a Python/ctypes argument to a (code, payload) the native
    bridge can push onto the call. Payload is an int (address/scalar), a
    float, or bytes."""
    if isinstance(value, _CArgObject):
        return ("P", value._address())
    if isinstance(value, (Array, Structure, Union)):
        # Aggregates are passed by the address of their own storage.
        return ("P", addressof(value))
    if isinstance(value, (_Pointer, CFuncPtr)):
        # Pointer-like scalars are passed *by value*: the callee receives the
        # address they hold (a target/code pointer), not the wrapper address.
        return ("P", int.from_bytes(value._read(0, _PTR), _BO))
    if isinstance(value, _SimpleCData):
        return (value._type_, value.value)
    if value is None:
        return ("P", 0)
    if isinstance(value, bool):
        return ("i", int(value))
    if isinstance(value, int):
        return ("P", value) if False else ("q", value)
    if isinstance(value, float):
        return ("d", value)
    if isinstance(value, bytes):
        return ("z", value)
    if isinstance(value, str):
        return ("Z", value)
    raise TypeError("cannot pass %r to a foreign function" % (type(value).__name__,))


def _ffi_invoke(addr, restype, argtypes, flags, args):
    if addr == 0:
        raise ValueError("attempt to call NULL function pointer")
    # Determine per-argument codes.
    codes = []
    payloads = []
    if argtypes:
        if len(args) != len(argtypes):
            raise TypeError(
                "this function takes %d argument(s) (%d given)"
                % (len(argtypes), len(args))
            )
        for at, val in zip(argtypes, args):
            conv = at.from_param(val) if hasattr(at, "from_param") else val
            code = _type_code_for_ffi(at)
            payload = _coerce_payload(code, conv if conv is not None else val)
            codes.append(code)
            payloads.append(payload)
    else:
        for val in args:
            code, payload = _arg_to_ffi(val)
            codes.append(code)
            payloads.append(payload)
    rcode = _type_code_for_ffi(restype)
    raw = _nat.call_function(addr, rcode, codes, payloads, int(flags))
    if restype is None:
        return None
    return _wrap_result(restype, raw)


def _coerce_payload(code, value):
    if code in ("P", "z", "Z", "O"):
        if isinstance(value, _CData):
            # Aggregates (struct/union/array) and py_object are passed by their
            # own address. Pointer-like scalars (c_char_p/c_wchar_p/c_void_p,
            # POINTER(...) and function pointers) are passed *by value*: the
            # callee receives the address they store, not the address of the
            # Python wrapper holding it.
            if code == "O" or isinstance(value, (Structure, Union, Array)):
                return addressof(value)
            return int.from_bytes(value._read(0, _PTR), _BO)
        if isinstance(value, _CArgObject):
            return value._address()
        if value is None:
            return 0
        if code == "z" and isinstance(value, (bytes, bytearray)):
            return value
        if code == "Z" and isinstance(value, str):
            return value
        return int(value)
    if code in _INT_CODES or code in ("c", "?"):
        if isinstance(value, _SimpleCData):
            return value.value
        if isinstance(value, (bytes, bytearray)) and code == "c":
            return value[0]
        return int(value)
    if code in ("f", "d", "g"):
        if isinstance(value, _SimpleCData):
            return float(value.value)
        return float(value)
    if isinstance(value, _SimpleCData):
        return value.value
    return value


def _wrap_result(restype, raw):
    if isinstance(restype, _CDataType) and issubclass(restype, _SimpleCData):
        # Native bridge returns a Python scalar already in `raw`.
        obj = restype()
        obj.value = raw
        return obj.value
    if isinstance(restype, _CDataType) and issubclass(restype, _Pointer):
        p = restype()
        p._write(0, int(raw).to_bytes(_PTR, _BO))
        return p
    return raw


def _from_closure_arg(argtype, raw):
    """Rebuild the declared ctypes argument a Python callback expects from the
    primitive the native trampoline delivers.

    The native bridge only knows single-character format codes, so a pointer
    argument arrives as a bare machine address (int), a ``char*``/``wchar_t*``
    as ``bytes``/``str``, and scalars as ``int``/``float``. For a typed
    pointer (``POINTER(T)``) CPython hands the callback a live pointer object,
    so reconstruct one over that address; every other declared type already
    matches the primitive the bridge produced.
    """
    if (
        argtype is not None
        and isinstance(argtype, _CDataType)
        and issubclass(argtype, _Pointer)
    ):
        p = argtype()
        p._write(0, int(raw).to_bytes(_PTR, _BO))
        return p
    return raw


def _to_closure_result(restype, result):
    """Reduce a callback's Python return value to the primitive the native
    trampoline writes back into the result register."""
    if restype is None:
        return None
    if isinstance(result, _SimpleCData):
        return result.value
    if isinstance(result, _Pointer):
        return int.from_bytes(result._read(0, _PTR), _BO)
    if isinstance(result, _CData):
        return addressof(result)
    return result


def _make_closure(funcptr, callable_):
    # A real C-callable closure is created by the native bridge. The native
    # trampoline can only marshal primitives, so wrap the user callable so it
    # (a) rebuilds each declared argtype (e.g. POINTER(c_int)) from the raw
    # primitive before the call and (b) reduces the return value back to a
    # primitive afterwards -- mirroring CPython's per-argument converters.
    functype = type(funcptr)
    argtypes = tuple(functype._argtypes_ or ())
    restype = functype._restype_
    argcodes = [_type_code_for_ffi(t) for t in argtypes]
    rcode = _type_code_for_ffi(restype)

    def _closure_entry(*raw):
        conv = [_from_closure_arg(at, val) for at, val in zip(argtypes, raw)]
        if len(raw) > len(argtypes):
            conv.extend(raw[len(argtypes):])
        return _to_closure_result(restype, callable_(*conv))

    try:
        return _nat.create_closure(_closure_entry, rcode, argcodes)
    except NotImplementedError:
        return 0
