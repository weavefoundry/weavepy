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
  errno, and the libffi-equivalent call/closure bridge.
* This module builds the ``_SimpleCData`` / ``Structure`` / ``Union`` /
  ``Array`` / ``_Pointer`` / ``CFuncPtr`` type system and its metaclasses
  (with CPython's exact names: ``PyCSimpleType``, ``PyCStructType``,
  ``UnionType``, ``PyCArrayType``, ``PyCPointerType``, ``PyCFuncPtrType``)
  on top of those primitives.

Faithfulness notes (all mirroring ``Modules/_ctypes``):

* Every concrete ctypes type carries a *StgInfo* (`_stginfo_` in the class
  dict), created by its metaclass ``__init__``. A type whose metaclass
  ``__init__`` never ran (e.g. built via ``Meta.__new__`` alone, or the
  abstract roots) has none, and instantiating it raises
  ``TypeError("abstract class")``. Re-running the metaclass ``__init__``
  raises ``SystemError`` ("already initialized").
* Keepalive bookkeeping is CPython's ``KeepRef``/``PyCData_GetContainer``
  protocol: `_objects` is ``None`` until first use, becomes either the kept
  object itself (types with no sub-objects) or a dict keyed by
  ``unique_key`` (``"<index>:<b_index>:..."`` hex chains).
* Struct/union fields are ``CField`` descriptor instances exposing
  ``.offset``/``.size``, disallowing instantiation and deletion.
* Memory model: owned storage is a Python ``bytearray`` (address-stable
  while un-resized); views share it at an offset; foreign memory is a raw
  integer address driven through ``read_mem``/``write_mem``.
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

# CPython Modules/_ctypes/callproc.c: maximum number of arguments in a
# foreign call or callback signature.
CTYPES_MAX_ARGCOUNT = 1024

_PTR = _nat.SIZEOF_VOID_P
_WCHAR = _nat.sizeof_code("u")
_BO = _sys.byteorder
_FP = "<" if _BO == "little" else ">"
_BO_SWAP = "big" if _BO == "little" else "little"
_FP_SWAP = ">" if _FP == "<" else "<"
# Multi-byte numeric ``_type_`` codes get a distinct opposite-endian sibling
# type (CPython names it ``<base>_be`` on a little-endian host); single-byte
# codes alias ``__ctype_le__``/``__ctype_be__`` to the type itself; all other
# codes (bool ``?``, the pointer/string ``P z Z O`` codes, ``u``) expose
# neither attribute — faithfully matching CPython's ``_ctypes``.
_SWAP_CODES = frozenset("hHiIlLqQfdv")
_SELF_ENDIAN_CODES = frozenset("bBc")
_ENDIAN_SUFFIX = "_be" if _BO == "little" else "_le"

# Function-pointer calling-convention / behaviour flags (ctypes.h values).
FUNCFLAG_CDECL = 0x1
FUNCFLAG_HRESULT = 0x2
FUNCFLAG_PYTHONAPI = 0x4
FUNCFLAG_USE_ERRNO = 0x8
FUNCFLAG_USE_LASTERROR = 0x10
FUNCFLAG_STDCALL = 0x0

TYPEFLAG_ISPOINTER = 0x100
TYPEFLAG_HASPOINTER = 0x200

# CPython type flags mirrored through the ``__flags__`` attribute so
# `test_ctypes`'s flag probes see the same bits the C implementation sets.
_TPFLAGS_DISALLOW_INSTANTIATION = 1 << 7
_TPFLAGS_IMMUTABLETYPE = 1 << 8


class ArgumentError(Exception):
    """Raised when a foreign function call gets an argument it can't
    convert (CPython exposes this from ``_ctypes``)."""


def get_errno():
    return _nat.get_errno()


def set_errno(value):
    return _nat.set_errno(value)


if _sys.platform == "darwin":
    def _dyld_shared_cache_contains_path(path):
        return _nat.dyld_shared_cache_contains_path(path)


if _sys.platform == "win32":
    # The nt-only surface ctypes/__init__.py imports inside its
    # `_os.name == "nt"` branches. All of it mirrors CPython's
    # Modules/_ctypes/callproc.c module methods.

    def get_last_error():
        """Return ctypes' *private* per-thread copy of ``LastError`` —
        the value the most recent ``use_last_error=True`` foreign call
        swapped out (callproc.c ``get_last_error`` reads ``space[1]``,
        never the thread's live ``GetLastError()``)."""
        return _nat.get_last_error()

    def set_last_error(value):
        """Set the private per-thread ``LastError`` copy, returning the
        previous value (it will be swapped *in* as the real ``LastError``
        for the next ``use_last_error=True`` foreign call)."""
        return _nat.set_last_error(value)

    def FormatError(code=None):
        """Message text for a Win32 error code (``FormatMessageW``); with
        no argument, the calling thread's real ``GetLastError()`` — exactly
        CPython's ``format_error`` (callproc.c)."""
        return _nat.format_error(code)

    def _check_HRESULT(result):
        # CPython's check_hresult (callproc.c) raises via
        # PyErr_SetFromWindowsErr when FAILED(hr) — i.e. the HRESULT is
        # negative as a signed 32-bit int — and returns the value
        # otherwise. We raise the same WinError-shaped OSError (winerror
        # carries the HRESULT). Divergence note: ctypes' *COMError* (an
        # HRESULT failure returned by a COM method call through a
        # FUNCFLAG_HRESULT prototype) does not exist in WeavePy; OleDLL
        # results route through this checker and get OSError instead.
        if result < 0:
            raise OSError(None, FormatError(result).strip(), None, result)
        return result

    def CopyComPointer(src, dst):
        """CPython implements this in Modules/_ctypes/callproc.c for COM
        interop (AddRef the source, store it through ``dst``). WeavePy has
        no COM object model, so this is a documented stub."""
        raise NotImplementedError(
            "COM pointers are not supported by WeavePy")

    def LoadLibrary(name, load_flags=0):
        """CPython's ``load_library`` (``LoadLibraryExW``-based,
        callproc.c). ``load_flags`` is ctypes' ``winmode``; the native
        loader currently applies plain ``LoadLibraryW`` default search
        semantics and ignores the flag bits (RFC 0063 documents the
        divergence)."""
        return _nat.dlopen(name, load_flags)

    def FreeLibrary(handle):
        _nat.dlclose(handle)


# ---------------------------------------------------------------------------
# StgInfo — per-type storage info (CPython's StgInfo struct)
# ---------------------------------------------------------------------------


class _StgInfo:
    __slots__ = (
        "size",         # total size in bytes
        "align",        # alignment requirement
        "length",       # number of sub-objects (fields/elements); 0 => the
                        # keepalive container stores a single object directly
        "final",        # _fields_ may no longer be (re)assigned
        "code",         # simple-type format char, or None
        "swapped",      # opposite-endian simple variant
        "proto",        # element type (arrays) / target type (pointers)
        "fields",       # dict name -> CField (aggregates), in layout order
        "flags",
        "format",       # PEP 3118 format string, or None (=> "B")
    )

    def __init__(self):
        self.size = 0
        self.align = 1
        self.length = 0
        self.final = False
        self.code = None
        self.swapped = False
        self.proto = None
        self.fields = None
        self.flags = 0
        self.format = None


_PEP_STD_SIZE = {"b": 1, "B": 1, "h": 2, "H": 2, "i": 4, "I": 4,
                 "l": 4, "L": 4, "q": 8, "Q": 8, "?": 1}


def _pep_simple_char(code, size):
    """PEP 3118 formats use *standard* struct-module sizes: an 8-byte
    C long is spelled "q", not "l" (cfield.c's format tables)."""
    std = _PEP_STD_SIZE.get(code)
    if std is None or std == size:
        return code
    if code in "bhilq":
        table = {1: "b", 2: "h", 4: "l", 8: "q"}
    else:
        table = {1: "B", 2: "H", 4: "L", 8: "Q"}
    return table.get(size, code)


def _info(cls):
    """The class's own StgInfo (CPython PyStgInfo_FromType) or None."""
    return cls.__dict__.get("_stginfo_")


def _info_req(cls):
    info = _info(cls)
    if info is None:
        raise TypeError("abstract class")
    return info


def _set_info(cls, info):
    type.__setattr__(cls, "_stginfo_", info)


def _check_not_initialized(cls):
    if "_stginfo_" in cls.__dict__:
        raise SystemError(
            "ctypes state of '%s' is already initialized" % (cls.__name__,)
        )


# ---------------------------------------------------------------------------
# Low-level value codecs for the simple ``_type_`` format codes
# ---------------------------------------------------------------------------

# Integer codes -> (size, signed). 'v' is VARIANT_BOOL (a 2-byte short).
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
    "v": (_nat.sizeof_code("h"), True),
}


def _index(value):
    """CPython's PyNumber_Index — ints and __index__ only (no floats)."""
    idx = getattr(type(value), "__index__", None)
    if idx is None:
        raise TypeError(
            "'%s' object cannot be interpreted as an integer"
            % (type(value).__name__,)
        )
    return idx(value)


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


def _simple_get(code, obj, off=0, swap=False):
    bo = _BO_SWAP if swap else _BO
    fp = _FP_SWAP if swap else _FP
    if code == "v":
        v = int.from_bytes(_read_at(obj, off, _INT_CODES["v"][0]), bo)
        return v != 0
    if code in _INT_CODES:
        size, signed = _INT_CODES[code]
        v = int.from_bytes(_read_at(obj, off, size), bo)
        if signed and v >= (1 << (size * 8 - 1)):
            v -= 1 << (size * 8)
        return v
    if code == "f":
        import struct as _struct
        return _struct.unpack(fp + "f", _read_at(obj, off, 4))[0]
    if code in ("d", "g"):
        import struct as _struct
        sz = _nat.sizeof_code(code)
        if sz == 8:
            return _struct.unpack(fp + "d", _read_at(obj, off, 8))[0]
        # x86's 80-bit long double: decode the extended format manually.
        return _long_double_get(_read_at(obj, off, sz))
    if code == "c":
        return _read_at(obj, off, 1)
    if code == "?":
        return _read_at(obj, off, 1)[0] != 0
    if code == "u":
        cp = int.from_bytes(_read_at(obj, off, _WCHAR), _BO)
        try:
            return chr(cp)
        except ValueError:
            return "\ufffd"
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
        mask = (1 << (8 * _PTR)) - 1
        root = obj
        while root._b_base_ is not None:
            root = root._b_base_
        found = _find_kept_by_id(root._objects, v, mask)
        if found is not _SENTINEL:
            return found
        raise ValueError("PyObject is NULL")
    raise TypeError("unknown type code %r" % code)


_SENTINEL = object()


def _find_kept_by_id(objs, marker, mask):
    if objs is None:
        return _SENTINEL
    if isinstance(objs, dict):
        for kept in objs.values():
            found = _find_kept_by_id(kept, marker, mask)
            if found is not _SENTINEL:
                return found
        return _SENTINEL
    if id(objs) & mask == marker:
        return objs
    return _SENTINEL


def _long_double_get(raw):
    # x86 80-bit extended precision, stored little-endian in 12/16 bytes.
    frac = int.from_bytes(raw[:8], "little")
    se = int.from_bytes(raw[8:10], "little")
    sign = -1.0 if se & 0x8000 else 1.0
    exp = se & 0x7FFF
    if exp == 0 and frac == 0:
        return 0.0 * sign
    return sign * (frac / (1 << 63)) * 2.0 ** (exp - 16383)


def _long_double_set(value):
    import math
    v = float(value)
    if v == 0.0 or math.isnan(v) or math.isinf(v):
        if math.isnan(v):
            se, frac = 0x7FFF, 0xC000000000000000
        elif math.isinf(v):
            se, frac = 0x7FFF, 0x8000000000000000
        else:
            se, frac = 0, 0
        if math.copysign(1.0, v) < 0:
            se |= 0x8000
    else:
        m, e = math.frexp(abs(v))
        # m in [0.5, 1): extended format wants mantissa in [1, 2).
        frac = int(m * (1 << 64))
        exp = e - 1 + 16383
        se = exp & 0x7FFF
        if v < 0:
            se |= 0x8000
    sz = _nat.sizeof_code("g")
    return frac.to_bytes(8, "little") + se.to_bytes(2, "little") + b"\x00" * (sz - 10)


def _simple_set(code, obj, value, off=0, swap=False):
    """Write ``value`` with CPython's setfunc semantics.

    Returns ``(keep, shadow)``: the object to record on the keepalive list
    (CPython setfuncs return the kept object or None) plus an optional
    private NUL-terminated backing buffer that must live exactly as long
    as the kept object (WeavePy bytes objects are not NUL-terminated in
    memory, so ``z``/``Z`` pointers aim at a shadow copy).
    """
    bo = _BO_SWAP if swap else _BO
    fp = _FP_SWAP if swap else _FP
    if code == "v":
        size, _ = _INT_CODES["v"]
        iv = (1 if value else 0) & ((1 << (size * 8)) - 1)
        _write_at(obj, off, iv.to_bytes(size, bo))
        return None, None
    if code in _INT_CODES:
        size, _signed = _INT_CODES[code]
        iv = _index(value) & ((1 << (size * 8)) - 1)
        _write_at(obj, off, iv.to_bytes(size, bo))
        return None, None
    if code == "f":
        import struct as _struct
        _write_at(obj, off, _struct.pack(fp + "f", _as_float(value)))
        return None, None
    if code in ("d", "g"):
        import struct as _struct
        sz = _nat.sizeof_code(code)
        if sz == 8:
            _write_at(obj, off, _struct.pack(fp + "d", _as_float(value)))
        else:
            _write_at(obj, off, _long_double_set(value))
        return None, None
    if code == "c":
        if isinstance(value, (bytes, bytearray)) and len(value) == 1:
            b = bytes(value)
        elif isinstance(value, int) and not isinstance(value, bool):
            if not 0 <= value < 256:
                raise TypeError(
                    "one character bytes, bytearray or integer expected"
                )
            b = bytes([value])
        else:
            raise TypeError("one character bytes, bytearray or integer expected")
        _write_at(obj, off, b)
        return None, None
    if code == "?":
        _write_at(obj, off, b"\x01" if value else b"\x00")
        return None, None
    if code == "u":
        if not isinstance(value, str):
            raise TypeError(
                "unicode string expected instead of %s instance"
                % type(value).__name__
            )
        if len(value) != 1:
            raise TypeError("one character unicode string expected")
        _write_at(obj, off, ord(value).to_bytes(_WCHAR, _BO))
        return None, None
    if code == "P":
        if value is None:
            iv = 0
        elif isinstance(value, int):
            iv = value & ((1 << (8 * _PTR)) - 1)
        else:
            raise TypeError("cannot be converted to pointer")
        _write_at(obj, off, iv.to_bytes(_PTR, _BO))
        return None, None
    if code == "z":
        if value is None:
            _write_at(obj, off, (0).to_bytes(_PTR, _BO))
            return None, None
        if isinstance(value, int):
            iv = value & ((1 << (8 * _PTR)) - 1)
            _write_at(obj, off, iv.to_bytes(_PTR, _BO))
            return None, None
        if isinstance(value, bytes):
            shadow = bytearray(value)
            shadow.append(0)
            iv = _nat.addressof_buffer(shadow)
            _write_at(obj, off, iv.to_bytes(_PTR, _BO))
            return value, shadow
        raise TypeError(
            "bytes or integer address expected instead of %s instance"
            % type(value).__name__
        )
    if code == "Z":
        if value is None:
            _write_at(obj, off, (0).to_bytes(_PTR, _BO))
            return None, None
        if isinstance(value, int):
            iv = value & ((1 << (8 * _PTR)) - 1)
            _write_at(obj, off, iv.to_bytes(_PTR, _BO))
            return None, None
        if isinstance(value, str):
            shadow = _wchar_buffer(value)
            iv = _nat.addressof_buffer(shadow)
            _write_at(obj, off, iv.to_bytes(_PTR, _BO))
            return value, shadow
        raise TypeError(
            "unicode string or integer address expected instead of %s instance"
            % type(value).__name__
        )
    if code == "O":
        marker = id(value) & ((1 << (8 * _PTR)) - 1)
        _write_at(obj, off, marker.to_bytes(_PTR, _BO))
        return value, None
    raise TypeError("unknown type code %r" % code)


def _as_float(value):
    # CPython d_set/f_set use PyFloat_AsDouble: floats, ints, __float__,
    # __index__; huge ints raise OverflowError (via float()).
    if isinstance(value, float):
        return value
    if isinstance(value, int):
        return float(value)
    conv = getattr(type(value), "__float__", None)
    if conv is not None:
        return conv(value)
    idx = getattr(type(value), "__index__", None)
    if idx is not None:
        return float(idx(value))
    raise TypeError(
        "must be real number, not %s" % (type(value).__name__,)
    )


def _wchar_buffer(value):
    kb = bytearray()
    for ch in value:
        kb += ord(ch).to_bytes(_WCHAR, _BO)
    kb += (0).to_bytes(_WCHAR, _BO)
    return kb


# ---------------------------------------------------------------------------
# Keepalive bookkeeping (CPython KeepRef / PyCData_GetContainer)
# ---------------------------------------------------------------------------


def _container_of(obj):
    """Walk ``b_base`` to the root and materialise its keepalive holder:
    a fresh dict for container types, else leave ``None`` (a subsequent
    KeepRef stores its object directly)."""
    root = obj
    while root._b_base_ is not None:
        root = root._b_base_
    if root._objects is None:
        info = _info(type(root))
        if info is not None and info.length > 0:
            root._objects = {}
    return root


def _get_keeped(obj):
    """CPython GetKeepedObjects: the root container's b_objects."""
    return _container_of(obj)._objects


def _unique_key(target, index):
    parts = [format(index & 0xFFFFFFFF, "x")]
    while target._b_base_ is not None:
        parts.append(format(target._b_index & 0xFFFFFFFF, "x"))
        target = target._b_base_
    return ":".join(parts)


def _keep_ref(target, index, keep, shadow=None):
    """CPython KeepRef: record ``keep`` on the root container, either
    directly (scalar containers) or under a unique hex key (dicts)."""
    if keep is None:
        return
    root = _container_of(target)
    objs = root._objects
    if not isinstance(objs, dict):
        root._objects = keep
        root._b_shadow = shadow
        return
    key = _unique_key(target, index)
    objs[key] = keep
    if shadow is not None:
        sh = root._b_shadow
        if not isinstance(sh, dict):
            sh = {}
            root._b_shadow = sh
        sh[key] = shadow


# ---------------------------------------------------------------------------
# Address helpers
# ---------------------------------------------------------------------------


def _as_address(x):
    """Coerce a Python value to an integer machine address (the c_void_p
    argument-conversion subset used internally)."""
    if x is None:
        return 0
    if isinstance(x, bool):
        return int(x)
    if isinstance(x, int):
        return x
    if isinstance(x, _CArgObject):
        return x._address()
    if isinstance(x, (_Pointer, CFuncPtr)):
        return int.from_bytes(x._read(0, _PTR), _BO)
    if isinstance(x, _SimpleCData):
        if _info_req(type(x)).code in ("P", "z", "Z", "O"):
            return int.from_bytes(x._read(0, _PTR), _BO)
        raise TypeError("cannot convert %r to an address" % (type(x).__name__,))
    if isinstance(x, _CData):
        return addressof(x)
    param = getattr(x, "_as_parameter_", None)
    if param is not None:
        return _as_address(param)
    raise TypeError("cannot convert %r to an address" % (type(x).__name__,))


def _addr_of(x):
    """`_as_address`, also accepting bytes-like objects (their data
    pointer) — what ``memmove`` / ``string_at`` / ``cast`` accept."""
    if isinstance(x, (bytes, bytearray)):
        return _nat.addressof_buffer(x)
    return _as_address(x)


# ---------------------------------------------------------------------------
# CField — struct/union field descriptor (CPython's CField type)
# ---------------------------------------------------------------------------


class CField:
    __slots__ = ("name", "type", "offset", "size", "index",
                 "bit_size", "bit_offset", "_swapped")
    __flags__ = _TPFLAGS_IMMUTABLETYPE | _TPFLAGS_DISALLOW_INSTANTIATION

    def __init__(self, *args, **kwargs):
        raise TypeError("cannot create 'CField' instances")

    def __repr__(self):
        bits = ""
        if self.bit_size is not None:
            bits = ", bit_size=%d, bit_offset=%d" % (self.bit_size,
                                                     self.bit_offset)
        return "<Field type=%s, ofs=%d%s, size=%d>" % (
            self.type.__name__, self.offset, bits, self.size)

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        if not isinstance(obj, _CData):
            raise TypeError("not a ctype instance")
        ftype = self.type
        finfo = _info_req(ftype)
        if self.bit_size is not None:
            return self._get_bits(obj)
        if finfo.code is not None and _is_direct_simple(ftype):
            return _simple_get(finfo.code, obj, self.offset, finfo.swapped)
        # Fields typed as c_char/c_wchar arrays read as bytes/str
        # (CPython installs s_get/U_get for them).
        akind = _char_array_kind(ftype)
        if akind == "c":
            data = _read_at(obj, self.offset, finfo.size)
            nul = data.find(b"\x00")
            return data if nul < 0 else data[:nul]
        if akind == "u":
            return _wchar_decode(_read_at(obj, self.offset, finfo.size))
        return _view(obj, ftype, self.offset, self.index)

    def __set__(self, obj, value):
        if not isinstance(obj, _CData):
            raise TypeError("not a ctype instance")
        ftype = self.type
        finfo = _info_req(ftype)
        if self.bit_size is not None:
            self._set_bits(obj, value)
            return
        if finfo.code is not None and _is_direct_simple(ftype):
            keep, shadow = _simple_set(
                finfo.code, obj, value, self.offset, finfo.swapped)
            _keep_ref(obj, self.index, keep, shadow)
            return
        akind = _char_array_kind(ftype)
        if akind == "c":
            # CPython s_set: strlen-bounded copy plus NUL if it fits.
            if not isinstance(value, bytes):
                raise TypeError(
                    "expected bytes, %s found" % (type(value).__name__,))
            nul = value.find(b"\x00")
            data = value if nul < 0 else value[:nul]
            if len(data) < finfo.size:
                data += b"\x00"
            elif len(data) > finfo.size:
                raise ValueError("byte string too long")
            _write_at(obj, self.offset, data)
            return
        if akind == "u":
            if not isinstance(value, str):
                raise TypeError(
                    "unicode string expected instead of %s instance"
                    % (type(value).__name__,))
            nchars = finfo.size // _WCHAR
            nul = value.find("\x00")
            data = value if nul < 0 else value[:nul]
            if len(data) > nchars:
                raise ValueError("string too long")
            raw = b"".join(ord(c).to_bytes(_WCHAR, _BO) for c in data)
            if len(data) < nchars:
                raw += (0).to_bytes(_WCHAR, _BO)
            _write_at(obj, self.offset, raw)
            return
        _cdata_set(obj, ftype, self.offset, self.index, value)

    def __delete__(self, obj):
        raise TypeError("cannot delete attribute")

    # -- bitfields --------------------------------------------------------

    def _unit(self, obj):
        finfo = _info(self.type)
        size, signed = _INT_CODES[finfo.code]
        bo = _BO_SWAP if finfo.swapped else _BO
        v = int.from_bytes(_read_at(obj, self.offset, size), bo)
        return v, size, signed, bo

    def _get_bits(self, obj):
        v, size, signed, _bo = self._unit(obj)
        v = (v >> self.bit_offset) & ((1 << self.bit_size) - 1)
        if signed and v >= (1 << (self.bit_size - 1)):
            v -= 1 << self.bit_size
        return v

    def _set_bits(self, obj, value):
        value = _index(value)
        v, size, _signed, bo = self._unit(obj)
        mask = (1 << self.bit_size) - 1
        v &= ~(mask << self.bit_offset)
        v |= (value & mask) << self.bit_offset
        _write_at(obj, self.offset, v.to_bytes(size, bo))


def _make_cfield(name, ftype, offset, size, index, bit_size=None,
                 bit_offset=0):
    fld = object.__new__(CField)
    object.__setattr__(fld, "name", name)
    object.__setattr__(fld, "type", ftype)
    object.__setattr__(fld, "offset", offset)
    object.__setattr__(fld, "size", size)
    object.__setattr__(fld, "index", index)
    object.__setattr__(fld, "bit_size", bit_size)
    object.__setattr__(fld, "bit_offset", bit_offset)
    object.__setattr__(fld, "_swapped", False)
    return fld


def _is_direct_simple(t):
    """CPython _ctypes_simple_instance is *false* (use the plain getfunc)
    only when the type's base is _SimpleCData itself; subclasses read back
    as instances sharing memory."""
    mro = t.__mro__
    return len(mro) > 1 and mro[1] is _SimpleCData


def _char_array_kind(t):
    if isinstance(t, PyCArrayType):
        einfo = _info(t.__dict__.get("_type_", None) or getattr(t, "_type_", None))
        if einfo is not None:
            if einfo.code == "c":
                return "c"
            if einfo.code == "u":
                return "u"
    return None


def _wchar_decode(raw):
    chars = []
    for i in range(0, len(raw), _WCHAR):
        cp = int.from_bytes(raw[i:i + _WCHAR], _BO)
        if cp == 0:
            break
        chars.append(chr(cp))
    return "".join(chars)


# ---------------------------------------------------------------------------
# Metaclasses (CPython names)
# ---------------------------------------------------------------------------


class _CDataMeta(type):
    """Shared behaviour of every ctypes metaclass (CPython's CDataType
    methods: they live on the metaclass so they are callable on the class)."""

    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __mul__(cls, length):
        return _create_array_type(cls, length)

    def __rmul__(cls, length):
        return _create_array_type(cls, length)

    # -- construction from existing memory -------------------------------

    def from_address(cls, address):
        _info_req(cls)
        inst = _blank(cls)
        object.__setattr__(inst, "_b_addr", _index(address))
        return inst

    def from_buffer(cls, source, offset=0):
        info = _info_req(cls)
        mv = memoryview(source)
        if mv.readonly:
            raise TypeError("underlying buffer is not writable")
        if not mv.c_contiguous:
            raise TypeError("underlying buffer is not C contiguous")
        if mv.ndim != 1 or mv.itemsize != 1:
            mv = mv.cast("B")
        if offset < 0:
            raise ValueError("offset cannot be negative")
        if mv.nbytes - offset < info.size:
            raise ValueError(
                "Buffer size too small (%d instead of at least %d bytes)"
                % (mv.nbytes, info.size + offset)
            )
        info.final = True
        inst = _blank(cls)
        object.__setattr__(inst, "_b_buffer", mv)
        object.__setattr__(inst, "_b_offset", offset)
        _keep_ref(inst, -1, mv)
        return inst

    def from_buffer_copy(cls, source, offset=0):
        info = _info_req(cls)
        if isinstance(source, str):
            raise TypeError(
                "a bytes-like object is required, not 'str'")
        # Release the temporary view eagerly: CPython's refcounting drops
        # its export the moment the C call returns, and test_frombuffer
        # asserts the source is resizable again right after the copy.
        with memoryview(source) as _mv:
            data = bytes(_mv)
        if offset < 0:
            raise ValueError("offset cannot be negative")
        if len(data) - offset < info.size:
            raise ValueError(
                "Buffer size too small (%d instead of at least %d bytes)"
                % (len(data), info.size + offset)
            )
        inst = _alloc_instance(cls)
        _write_at(inst, 0, data[offset:offset + info.size])
        return inst

    def in_dll(cls, dll, name):
        try:
            addr = _nat.dlsym(dll._handle, name)
        except OSError:
            addr = 0
        if not addr:
            raise ValueError("%s: symbol not found" % name)
        return cls.from_address(addr)

    def from_param(cls, value):
        if isinstance(value, cls):
            return value
        param = getattr(value, "_as_parameter_", None)
        if param is not None:
            return cls.from_param(param)
        raise TypeError(
            "expected %s instance instead of %s"
            % (cls.__name__, type(value).__name__)
        )


def _make_swapped_simple(cls, code):
    """Create the opposite-endian sibling of a multi-byte numeric type
    (CPython CreateSwappedType): same bases, same ``_type_``, plus the
    private ``_swapped_`` marker."""
    swapped = PyCSimpleType(
        cls.__name__ + _ENDIAN_SUFFIX,
        cls.__bases__,
        {"_type_": code, "_swapped_": True, "__module__": "ctypes"},
    )
    if _BO == "little":
        type.__setattr__(swapped, "__ctype_le__", cls)
        type.__setattr__(swapped, "__ctype_be__", swapped)
    else:
        type.__setattr__(swapped, "__ctype_be__", cls)
        type.__setattr__(swapped, "__ctype_le__", swapped)
    return swapped


class PyCSimpleType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        if namespace.get("_b_root_"):
            return  # _SimpleCData itself stays abstract
        code = getattr(cls, "_type_", None)
        if code is None:
            raise AttributeError("class must define a '_type_' attribute")
        if not isinstance(code, str) or len(code) != 1:
            raise ValueError(
                "class must define a '_type_' string attribute of length 1"
            )
        if code not in "cbBhHiIlLqQdfguzZPXOv?":
            raise AttributeError(
                "class must define a '_type_' attribute which must be\n"
                "a single character string containing one of "
                "'cbBhHiIlLqQdfguzZPXOv?'."
            )
        info = _StgInfo()
        info.code = code
        info.size = _nat.sizeof_code(code)
        info.align = _nat.alignment_code(code)
        info.swapped = bool(namespace.get("_swapped_"))
        info.format = ((">" if _BO == "little" else "<")
                       if info.swapped else _FP) \
            + _pep_simple_char(code, info.size)
        _set_info(cls, info)
        # Install CPython's __ctype_le__/__ctype_be__ endian aliases.
        if not info.swapped:
            if code in _SWAP_CODES:
                swapped = _make_swapped_simple(cls, code)
                if _BO == "little":
                    type.__setattr__(cls, "__ctype_le__", cls)
                    type.__setattr__(cls, "__ctype_be__", swapped)
                else:
                    type.__setattr__(cls, "__ctype_be__", cls)
                    type.__setattr__(cls, "__ctype_le__", swapped)
            elif code in _SELF_ENDIAN_CODES:
                type.__setattr__(cls, "__ctype_le__", cls)
                type.__setattr__(cls, "__ctype_be__", cls)

    def from_param(cls, value):
        # Exact instances pass through unchanged.
        if isinstance(value, cls):
            return value
        info = _info_req(cls)
        code = info.code
        try:
            return _simple_param(cls, code, value)
        except TypeError as exc:
            param = getattr(value, "_as_parameter_", None)
            if param is None:
                raise
            try:
                return _simple_param(cls, code, param)
            except TypeError:
                raise exc


def _simple_param(cls, code, value):
    # A bytes/str argument marshals to a pointer that CPython aims *into
    # the object's own buffer* — valid for the object's lifetime, and
    # callees legally stash it past the call (lxml compares a capsule
    # context set by an earlier PyCapsule_SetContext(cap, b"...")).
    # `intern_buffer` returns a process-lifetime deduplicated copy, so
    # the pointer never dangles (RFC 0076 WS3).
    if code == "z":
        if value is None:
            return None
        if isinstance(value, bytes):
            parg = _new_parg("z", value, value)
            parg._value = _nat.intern_buffer(value + b"\0")
            return parg
        if isinstance(value, _SimpleCData) and \
                _info_req(type(value)).code in ("z", "P"):
            return value
        if isinstance(value, (_Pointer, Array)):
            return value
        raise TypeError("wrong type")
    if code == "Z":
        if value is None:
            return None
        if isinstance(value, str):
            parg = _new_parg("Z", value, value)
            parg._value = _nat.intern_buffer(bytes(_wchar_buffer(value)))
            return parg
        if isinstance(value, _SimpleCData) and \
                _info_req(type(value)).code in ("Z", "P"):
            return value
        if isinstance(value, (_Pointer, Array)):
            return value
        raise TypeError("wrong type")
    if code == "P":
        if value is None:
            return None
        if isinstance(value, int):
            return _new_parg("P", value, value)
        if isinstance(value, bytes):
            parg = _new_parg("z", value, value)
            parg._value = _nat.intern_buffer(value + b"\0")
            return parg
        if isinstance(value, str):
            parg = _new_parg("Z", value, value)
            parg._value = _nat.intern_buffer(bytes(_wchar_buffer(value)))
            return parg
        if isinstance(value, _CArgObject):
            return value
        if isinstance(value, (_Pointer, Array, CFuncPtr)):
            return value
        if isinstance(value, _SimpleCData) and \
                _info_req(type(value)).code in ("z", "Z", "P"):
            return value
        raise TypeError("wrong type")
    # Numeric / char / bool codes: validate by writing into a fresh
    # instance, and pass a cparam wrapping it.
    inst = _alloc_instance(cls)
    keep, shadow = _simple_set(code, inst, value, 0, False)
    _keep_ref(inst, 0, keep, shadow)
    parg = _new_parg(code, inst, _simple_get(code, inst, 0, False))
    return parg


class PyCStructType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        _aggregate_init(cls, bases, namespace, union=False)

    def __setattr__(cls, key, value):
        if key == "_fields_":
            _set_fields(cls, value, union=False)
        else:
            type.__setattr__(cls, key, value)


class UnionType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        _aggregate_init(cls, bases, namespace, union=True)

    def __setattr__(cls, key, value):
        if key == "_fields_":
            _set_fields(cls, value, union=True)
        else:
            type.__setattr__(cls, key, value)


class PyCArrayType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        if namespace.get("_b_root_"):
            return  # Array itself stays abstract
        etype = getattr(cls, "_type_", None)
        if etype is None:
            raise AttributeError("class must define a '_type_' attribute")
        length = cls.__dict__.get("_length_", None)
        if length is None:
            length = getattr(cls, "_length_", None)
        if length is None:
            raise AttributeError(
                "class must define a '_length_' attribute")
        if isinstance(length, bool) or not isinstance(length, int):
            raise TypeError(
                "The '_length_' attribute must be an integer")
        if length < 0:
            raise ValueError(
                "The '_length_' attribute must not be negative")
        if length > _sys.maxsize:
            raise OverflowError(
                "The '_length_' attribute is too large")
        if not isinstance(etype, _CDataMeta):
            raise TypeError(
                "_type_ must have storage info")
        einfo = _info_req(etype)
        einfo.final = True
        total = einfo.size * length
        if total > _sys.maxsize:
            raise OverflowError("array too large")
        info = _StgInfo()
        info.size = total
        info.align = einfo.align
        info.length = length
        info.proto = etype
        # PEP 3118: nested array dims fold into one parenthesised group
        # ("(2,3)<f" for c_float*3*2).
        dims = [length]
        t = etype
        while isinstance(t, PyCArrayType):
            ti = _info_req(t)
            dims.append(ti.length)
            t = ti.proto
        base = _info_req(t).format or "B"
        info.format = "(" + ",".join(str(d) for d in dims) + ")" + base
        _set_info(cls, info)


class PyCPointerType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        if namespace.get("_b_root_"):
            return  # _Pointer itself stays abstract
        info = _StgInfo()
        info.size = _PTR
        info.align = _PTR
        info.length = 2
        info.proto = namespace.get("_type_", getattr(cls, "_type_", None))
        if info.proto is not None:
            if not isinstance(info.proto, type) or not issubclass(info.proto, _CData):
                raise TypeError("_type_ must be a type")
            # NB: creating a pointer type does NOT finalize the target —
            # `POINTER(Incomplete)` before `_fields_` assignment is the
            # documented forward-reference idiom (test_pep3118).
        # PEP 3118 pointer format is snapshotted at creation time (an
        # incomplete target that is completed later keeps "&B" — see the
        # "not fixed" remark in test_pep3118).
        tinfo = _info(info.proto) if isinstance(info.proto, type) else None
        info.format = "&" + ((tinfo.format or "B") if tinfo is not None else "B")
        _set_info(cls, info)

    def set_type(cls, t):
        info = _info_req(cls)
        info.proto = t
        type.__setattr__(cls, "_type_", t)

    def from_param(cls, value):
        if value is None:
            return None
        info = _info_req(cls)
        tgt = info.proto
        if isinstance(value, cls):
            return value
        # A bare <type> instance where POINTER(<type>) is declared is
        # accepted by reference, as CPython's PyCPointerType_from_param
        # does (polars' cpuid thunk passes its struct straight in).
        if tgt is not None and isinstance(value, tgt):
            return byref(value)
        if isinstance(value, _CArgObject):
            obj = value._obj
            if isinstance(obj, _CData) and type(obj) is tgt:
                return value
            raise TypeError(
                "expected %s instance instead of pointer to %s"
                % (cls.__name__, type(obj).__name__))
        if isinstance(value, (_Pointer, Array)):
            vinfo = _info_req(type(value))
            if vinfo.proto is tgt:
                return value
            raise TypeError(
                "expected %s instance instead of %s"
                % (cls.__name__, type(value).__name__))
        param = getattr(value, "_as_parameter_", None)
        if param is not None:
            return cls.from_param(param)
        raise TypeError(
            "expected %s instance instead of %s"
            % (cls.__name__, type(value).__name__))


class PyCFuncPtrType(_CDataMeta):
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __init__(cls, name, bases, namespace, **kw):
        _check_not_initialized(cls)
        if namespace.get("_b_root_"):
            return  # CFuncPtr itself stays abstract
        argtypes = getattr(cls, "_argtypes_", None)
        if argtypes is not None:
            if len(argtypes) > CTYPES_MAX_ARGCOUNT:
                raise ArgumentError(
                    "too many arguments (%d), maximum is %d"
                    % (len(argtypes), CTYPES_MAX_ARGCOUNT))
            for i, at in enumerate(argtypes):
                if not hasattr(at, "from_param"):
                    raise TypeError(
                        "item %d in _argtypes_ has no from_param method"
                        % (i + 1,))
        flags = getattr(cls, "_flags_", None)
        if not isinstance(flags, int):
            raise TypeError(
                "class must define _flags_ which must be an integer")
        info = _StgInfo()
        info.size = _PTR
        info.align = _PTR
        info.length = 1
        info.format = "X{}"  # PEP 3118: function signatures unimplemented
        _set_info(cls, info)


# ---------------------------------------------------------------------------
# Aggregate layout
# ---------------------------------------------------------------------------


def _aggregate_init(cls, bases, namespace, union):
    """Metaclass __init__ half of PyCStructUnionType_init: create the
    stginfo (inheriting the base layout) and, if the class body supplied
    `_fields_`, route it through normal attribute assignment so Python
    metaclass overrides (ctypes/_endian.py) participate."""
    base = bases[0] if bases else None
    if namespace.get("_b_root_"):
        return  # Structure / Union themselves stay abstract
    info = _StgInfo()
    binfo = _info(base) if isinstance(base, type) else None
    if binfo is not None:
        binfo.final = True  # subclassing finalizes the base (test_4)
        info.size = binfo.size
        info.align = binfo.align
        info.length = binfo.length
        info.fields = dict(binfo.fields) if binfo.fields else {}
    else:
        info.fields = {}
    _set_info(cls, info)
    if "_fields_" in namespace:
        cls._fields_ = namespace["_fields_"]
    elif "_anonymous_" in namespace:
        # _anonymous_ without _fields_: every name is "not in _fields_".
        anon = namespace["_anonymous_"]
        if not isinstance(anon, (list, tuple)):
            raise TypeError("_anonymous_ must be a sequence")
        for n in anon:
            raise AttributeError(
                "'%s' is specified in _anonymous_ but not in _fields_" % (n,))


def _set_fields(cls, value, union):
    info = _info(cls)
    if info is None:
        raise TypeError("ctypes state is not initialized")
    if info.final:
        raise AttributeError("_fields_ is final")
    fields = list(value)

    pack = getattr(cls, "_pack_", 0)
    if pack:
        if isinstance(pack, bool) or not isinstance(pack, int) or pack < 0:
            raise ValueError("_pack_ must be a non-negative integer")
    forced_align = getattr(cls, "_align_", 1)
    if isinstance(forced_align, bool) or not isinstance(forced_align, int):
        raise TypeError("_align_ must be a non-negative integer")
    if forced_align < 0:
        raise ValueError("_align_ must be a non-negative integer")
    forced_align = max(forced_align, 1)

    base = cls.__mro__[1] if len(cls.__mro__) > 1 else None
    binfo = _info(base) if isinstance(base, type) else None
    if binfo is not None:
        base_size = binfo.size
        base_align = binfo.align
        base_fields = dict(binfo.fields) if binfo.fields else {}
        base_len = binfo.length
    else:
        base_size = 0
        base_align = 1
        base_fields = {}
        base_len = 0

    layout = dict(base_fields)
    offset = 0 if union else base_size
    total_align = max(base_align, forced_align)
    max_size = base_size
    index = base_len
    # Open bitfield storage unit: (unit_offset, unit_size, bits_used).
    bit_state = None

    for i, item in enumerate(fields):
        try:
            fname = item[0]
            ftype = item[1]
        except (TypeError, IndexError):
            raise TypeError(
                "'_fields_' must be a sequence of (name, C type) pairs")
        bits = item[2] if len(item) > 2 else None
        if not isinstance(fname, str):
            raise TypeError(
                "first item in _fields_ tuple (index %d) must be a string"
                % (i,))
        if not isinstance(ftype, _CDataMeta) or _info(ftype) is None:
            raise TypeError(
                "second item in _fields_ tuple (index %d) must be a C type"
                % (i,))
        finfo = _info(ftype)
        finfo.final = True  # using a type as a field finalizes it (test_3)
        fsize = finfo.size
        falign = finfo.align
        if pack:
            falign = min(falign, pack)

        if bits is not None:
            if finfo.code not in _INT_CODES:
                raise TypeError(
                    "bit fields not allowed for type %s" % (ftype.__name__,))
            if isinstance(bits, bool) or not isinstance(bits, int) or \
                    not 0 < bits <= fsize * 8:
                raise ValueError(
                    "number of bits invalid for bit field %r" % (fname,))
            if union:
                foffset = 0
                bit_off = 0
                max_size = max(max_size, fsize)
            elif (bit_state is not None
                    and bit_state[1] == fsize
                    and bit_state[2] + bits <= fsize * 8):
                foffset = bit_state[0]
                bit_off = bit_state[2]
                bit_state = (foffset, fsize, bit_off + bits)
            else:
                offset = _round_up(offset, falign)
                foffset = offset
                offset += fsize
                bit_off = 0
                bit_state = (foffset, fsize, bits)
            total_align = max(total_align, falign)
            fld = _make_cfield(fname, ftype, foffset, fsize, index,
                               bits, bit_off)
            layout[fname] = fld
            type.__setattr__(cls, fname, fld)
            index += 1
            continue

        bit_state = None
        if union:
            foffset = 0
            max_size = max(max_size, fsize)
        else:
            offset = _round_up(offset, falign)
            foffset = offset
            offset += fsize
        total_align = max(total_align, falign)
        fld = _make_cfield(fname, ftype, foffset, fsize, index)
        layout[fname] = fld
        type.__setattr__(cls, fname, fld)
        index += 1

    if union:
        total = _round_up(max_size, total_align)
    else:
        total = _round_up(offset, total_align)
    if total > _sys.maxsize:
        raise OverflowError("structure or union is too large")

    info.fields = layout
    info.size = total
    info.align = total_align
    info.length = index
    info.final = True
    info.format = _pep_struct_format(info, union)
    type.__setattr__(cls, "_fields_", fields)
    _make_anon_fields(cls, info)


def _pep_struct_format(info, union):
    """PEP 3118 "T{...}" format built from the final layout (CPython
    stgdict.c). Unions and bitfields aren't expressible — fall back to
    the unstructured "B" byte view."""
    if union:
        return "B"
    parts = []
    pos = 0
    for name, fld in info.fields.items():
        if fld.bit_size is not None:
            return "B"
        if fld.offset > pos:
            n = fld.offset - pos
            parts.append("x" if n == 1 else "%dx" % n)
        finfo = _info(fld.type)
        parts.append((finfo.format if finfo is not None else None) or "B")
        parts.append(":%s:" % name)
        pos = fld.offset + fld.size
    if info.size > pos:
        n = info.size - pos
        parts.append("x" if n == 1 else "%dx" % n)
    return "T{" + "".join(parts) + "}"


def _round_up(n, align):
    if align <= 1:
        return n
    rem = n % align
    return n if rem == 0 else n + (align - rem)


def _make_anon_fields(cls, info):
    anon = getattr(cls, "_anonymous_", None)
    if anon is None:
        return
    if not isinstance(anon, (list, tuple)):
        raise TypeError("_anonymous_ must be a sequence")
    for name in anon:
        outer = info.fields.get(name)
        if outer is None:
            raise AttributeError(
                "'%s' is specified in _anonymous_ but not in _fields_"
                % (name,))
        inner_info = _info(outer.type)
        if inner_info is None or inner_info.fields is None:
            raise TypeError(
                "'%s' is specified in _anonymous_ but is not a structure "
                "or union" % (name,))
        for iname, ifld in inner_info.fields.items():
            promoted = _make_cfield(
                iname, ifld.type, outer.offset + ifld.offset, ifld.size,
                outer.index, ifld.bit_size, ifld.bit_offset)
            info.fields[iname] = promoted
            type.__setattr__(cls, iname, promoted)


# ---------------------------------------------------------------------------
# Instance allocation helpers
# ---------------------------------------------------------------------------


def _blank(cls):
    """A bare instance with default memory slots (address 0, no buffer)."""
    inst = object.__new__(cls)
    object.__setattr__(inst, "_b_buffer", None)
    object.__setattr__(inst, "_b_offset", 0)
    object.__setattr__(inst, "_b_addr", 0)
    object.__setattr__(inst, "_b_base_", None)
    object.__setattr__(inst, "_b_index", 0)
    object.__setattr__(inst, "_b_size", _info_req(cls).size)
    object.__setattr__(inst, "_objects", None)
    object.__setattr__(inst, "_b_shadow", None)
    return inst


def _alloc_instance(cls):
    """An instance of ``cls`` backed by fresh zeroed owned memory.
    Instantiation finalizes the class (test_2)."""
    info = _info_req(cls)
    info.final = True
    inst = _blank(cls)
    object.__setattr__(inst, "_b_buffer", bytearray(info.size))
    return inst


def _view(parent, ftype, field_offset, index):
    """A sub-object of ``ftype`` aliasing ``parent``'s memory at an offset
    (CPython PyCData_FromBaseObj)."""
    inst = _blank(ftype)
    if parent._b_buffer is not None:
        object.__setattr__(inst, "_b_buffer", parent._b_buffer)
        object.__setattr__(inst, "_b_offset",
                           parent._b_offset + field_offset)
    else:
        object.__setattr__(inst, "_b_addr", parent._b_addr + field_offset)
    object.__setattr__(inst, "_b_base_", parent)
    object.__setattr__(inst, "_b_index", index)
    return inst


def _cdata_set(dst, ftype, offset, index, value):
    """Generic aggregate/pointer field assignment (CPython _PyCData_set)."""
    finfo = _info_req(ftype)
    if value is None and issubclass(ftype, _Pointer):
        _write_at(dst, offset, (0).to_bytes(_PTR, _BO))
        return
    if isinstance(value, ftype):
        _write_at(dst, offset, value._read(0, finfo.size))
        _keep_ref(dst, index, _get_keeped(value))
        return
    if issubclass(ftype, _Pointer) and isinstance(value, Array):
        vinfo = _info_req(type(value))
        if vinfo.proto is not finfo.proto:
            raise TypeError(
                "incompatible types, %s instance instead of %s instance"
                % (type(value).__name__, ftype.__name__))
        _write_at(dst, offset, addressof(value).to_bytes(_PTR, _BO))
        _keep_ref(dst, index, value)
        return
    if issubclass(ftype, CFuncPtr) and callable(value):
        tmp = ftype(value)
        _write_at(dst, offset, tmp._read(0, finfo.size))
        _keep_ref(dst, index, tmp)
        return
    if isinstance(value, (list, tuple)) and not issubclass(ftype, _Pointer):
        tmp = ftype(*value)
        _write_at(dst, offset, tmp._read(0, finfo.size))
        _keep_ref(dst, index, _get_keeped(tmp))
        return
    raise TypeError(
        "incompatible types, %s instance instead of %s instance"
        % (type(value).__name__, ftype.__name__))


# ---------------------------------------------------------------------------
# Base data classes
# ---------------------------------------------------------------------------


class _CData(metaclass=type):
    # The internal storage slots (CPython keeps these in the CDataObject C
    # struct). Declaring them as __slots__ makes instances of fully slotted
    # subclass chains dict-less, exactly like CPython's C instances
    # (test_byteswap.test_slots).
    __slots__ = ("_b_buffer", "_b_offset", "_b_addr", "_b_base_",
                 "_b_index", "_b_size", "_objects", "_b_shadow",
                 "__weakref__")
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __new__(cls, *args, **kw):
        return _alloc_instance(cls)

    def __init__(self, *args, **kw):
        pass

    @property
    def _b_needsfree_(self):
        return 1 if (self._b_base_ is None
                     and isinstance(self._b_buffer, bytearray)) else 0

    # -- memory helpers --------------------------------------------------

    def _addr(self):
        buf = self._b_buffer
        if buf is not None:
            return _nat.addressof_buffer(buf) + self._b_offset
        return self._b_addr

    def _read(self, off, n):
        return _read_at(self, off, n)

    def _write(self, off, data):
        _write_at(self, off, data)

    def __buffer__(self, flags):
        # PEP 688 export of the object's live memory (CPython's
        # `PyCData_NewGetBuffer`). Only objects backed by an owned/shared
        # buffer can export; `from_address` objects wrap raw foreign
        # memory that a Python-level memoryview cannot alias.
        buf = self._b_buffer
        if buf is None:
            raise TypeError(
                "cannot create memoryview of a ctypes object backed by "
                "foreign memory"
            )
        start = self._b_offset
        end = start + self._b_size
        if isinstance(buf, memoryview):
            mv = buf[start:end]
        else:
            mv = memoryview(buf)[start:end]
        # Stamp the PEP 3118 metadata (CPython PyCData_NewGetBuffer):
        # arrays export their dims as `shape` with the element's format;
        # everything else is a 0-dim scalar of its own format.
        dims = []
        t = type(self)
        while isinstance(t, PyCArrayType):
            ti = _info_req(t)
            dims.append(ti.length)
            t = ti.proto
        ti = _info(t)
        fmt = (ti.format if ti is not None else None) or "B"
        itemsize = ti.size if ti is not None else 1
        _nat.configure_view(mv, fmt, itemsize,
                            tuple(dims) if dims else None)
        return mv

    def __ctypes_from_outparam__(self):
        return self

    def __reduce__(self):
        info = _info_req(type(self))
        if info.code in ("O", "P", "z", "Z") or issubclass(
                type(self), (_Pointer, CFuncPtr)) or (
                info.fields and any(
                    _info(f.type) is not None and _info(f.type).code in
                    ("O", "P", "z", "Z") for f in info.fields.values())):
            raise ValueError(
                "ctypes objects containing pointers cannot be pickled")
        return (_unpickle,
                (type(self),
                 (dict_or_empty(self), self._read(0, self._b_size))))

    def __setstate__(self, state):
        d, data = state
        for k, v in d.items():
            object.__setattr__(self, k, v)
        n = min(len(data), self._b_size)
        _write_at(self, 0, data[:n])
        return self


def dict_or_empty(obj):
    d = getattr(obj, "__dict__", None)
    return dict(d) if d else {}


def _unpickle(cls, state):
    inst = _alloc_instance(cls)
    inst.__setstate__(state)
    return inst


class _SimpleCData(_CData, metaclass=PyCSimpleType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args):
        if len(args) > 1:
            raise TypeError("call takes at most 1 argument (%d given)"
                            % (len(args),))
        if args:
            self.value = args[0]

    @property
    def value(self):
        info = _info_req(type(self))
        return _simple_get(info.code, self, 0, info.swapped)

    @value.setter
    def value(self, v):
        info = _info_req(type(self))
        keep, shadow = _simple_set(info.code, self, v, 0, info.swapped)
        _keep_ref(self, 0, keep, shadow)

    @value.deleter
    def value(self):
        raise TypeError("can't delete attribute")

    def __ctypes_from_outparam__(self):
        if _is_direct_simple(type(self)):
            return self.value
        return self

    def __repr__(self):
        if not _is_direct_simple(type(self)):
            # CPython's Simple_repr prints the *short* type name for
            # subclasses ("<X object at ...>"), not the qualified one.
            return "<%s object at 0x%012x>" % (type(self).__name__, id(self))
        return "%s(%r)" % (type(self).__name__, self.value)

    def __bool__(self):
        return any(self._read(0, self._b_size))

    def __eq__(self, other):
        if isinstance(other, _SimpleCData):
            return self.value == other.value
        return self.value == other

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return hash(self.value)


# -- Structure / Union -------------------------------------------------------


class Structure(_CData, metaclass=PyCStructType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args, **kw):
        info = _info_req(type(self))
        if args:
            names = list(info.fields.keys()) if info.fields else []
            if len(args) > len(names):
                raise TypeError("too many initializers")
            for i, val in enumerate(args):
                setattr(self, names[i], val)
        for key, val in kw.items():
            setattr(self, key, val)


class Union(_CData, metaclass=UnionType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args, **kw):
        info = _info_req(type(self))
        if args:
            names = list(info.fields.keys()) if info.fields else []
            if len(args) > len(names):
                raise TypeError("too many initializers")
            for i, val in enumerate(args):
                setattr(self, names[i], val)
        for key, val in kw.items():
            setattr(self, key, val)


# -- Array -------------------------------------------------------------------

_array_cache = {}


def _create_array_type(element_type, length):
    if isinstance(length, bool) or not isinstance(length, int):
        raise TypeError("can't multiply a ctypes type by a non-integer")
    if length < 0:
        raise ValueError("Array length must be >= 0, not %d" % length)
    if not isinstance(element_type, _CDataMeta):
        raise TypeError("Expected a ctypes type")
    einfo = _info_req(element_type)
    if einfo.size and length > _sys.maxsize // einfo.size:
        raise OverflowError("array too large")
    key = (element_type, length)
    cached = _array_cache.get(key)
    if cached is not None:
        return cached
    name = "%s_Array_%d" % (element_type.__name__, length)
    arr = PyCArrayType(name, (Array,),
                       {"_type_": element_type, "_length_": length})
    _array_cache[key] = arr
    return arr


class Array(_CData, metaclass=PyCArrayType):
    __slots__ = ()
    _b_root_ = True

    def __class_getitem__(cls, item):
        # CPython: `ctypes.Array.__class_getitem__ = Py_GenericAlias`.
        import types

        return types.GenericAlias(cls, item)

    def __init__(self, *args):
        if args:
            info = _info_req(type(self))
            if len(args) > info.length:
                raise IndexError("invalid index")
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
        info = _info_req(type(self))
        etype = info.proto
        einfo = _info_req(etype)
        esize = einfo.size
        if isinstance(index, slice):
            rng = range(*index.indices(info.length))
            if einfo.code == "c" and _is_direct_simple(etype):
                return bytes(self._read(i * esize, 1)[0] for i in rng)
            if einfo.code == "u" and _is_direct_simple(etype):
                return "".join(
                    _simple_get("u", self, i * esize, einfo.swapped)
                    for i in rng)
            return [self[i] for i in rng]
        index = self._check_index(_index(index))
        if einfo.code is not None and _is_direct_simple(etype):
            return _simple_get(einfo.code, self, index * esize,
                               einfo.swapped)
        return _view(self, etype, index * esize, index)

    def __setitem__(self, index, value):
        info = _info_req(type(self))
        etype = info.proto
        einfo = _info_req(etype)
        esize = einfo.size
        if isinstance(index, slice):
            rng = range(*index.indices(info.length))
            if len(value) != len(rng):
                raise ValueError(
                    "Can only assign sequence of same size")
            for i, v in zip(rng, value):
                self[i] = v
            return
        index = self._check_index(_index(index))
        if einfo.code is not None and _is_direct_simple(etype):
            # An instance of the element type is copied bitwise (CPython's
            # PyCData_set memcpy branch) — test_numeric_arrays stores
            # c_int() objects into a c_int array.
            if isinstance(value, etype):
                self._write(index * esize, value._read(0, esize))
                _keep_ref(self, index, _get_keeped(value))
                return
            keep, shadow = _simple_set(
                einfo.code, self, value, index * esize, einfo.swapped)
            _keep_ref(self, index, keep, shadow)
            return
        _cdata_set(self, etype, index * esize, index, value)

    def __delitem__(self, index):
        raise TypeError(
            "%s object doesn't support item deletion"
            % (type(self).__name__,))

    def __iter__(self):
        for i in range(len(self)):
            yield self[i]

    @property
    def value(self):
        kind = _char_array_kind(type(self))
        if kind == "c":
            data = self._read(0, self._b_size)
            nul = data.find(b"\x00")
            return data if nul < 0 else data[:nul]
        if kind == "u":
            return _wchar_decode(self._read(0, self._b_size))
        raise AttributeError(
            "'%s' object has no attribute 'value'" % (type(self).__name__,))

    @value.setter
    def value(self, val):
        kind = _char_array_kind(type(self))
        if kind == "c":
            if not isinstance(val, bytes):
                raise TypeError(
                    "bytes expected instead of %s instance"
                    % (type(val).__name__,))
            size = self._b_size
            if len(val) > size:
                raise ValueError("byte string too long")
            _write_at(self, 0, val)
            if len(val) < size:
                _write_at(self, len(val), b"\x00")
            return
        if kind == "u":
            if not isinstance(val, str):
                raise TypeError(
                    "unicode string expected instead of %s instance"
                    % (type(val).__name__,))
            nchars = self._b_size // _WCHAR
            if len(val) > nchars:
                raise ValueError("string too long")
            raw = b"".join(ord(c).to_bytes(_WCHAR, _BO) for c in val)
            if len(val) < nchars:
                raw += (0).to_bytes(_WCHAR, _BO)
            _write_at(self, 0, raw)
            return
        raise AttributeError(
            "'%s' object has no attribute 'value'" % (type(self).__name__,))

    @value.deleter
    def value(self):
        raise TypeError("can't delete attribute")

    @property
    def raw(self):
        if _char_array_kind(type(self)) != "c":
            raise AttributeError(
                "'%s' object has no attribute 'raw'"
                % (type(self).__name__,))
        return self._read(0, self._b_size)

    @raw.setter
    def raw(self, val):
        if _char_array_kind(type(self)) != "c":
            raise AttributeError(
                "'%s' object has no attribute 'raw'"
                % (type(self).__name__,))
        data = bytes(memoryview(val))
        if len(data) > self._b_size:
            raise ValueError("byte string too long")
        _write_at(self, 0, data)

    @raw.deleter
    def raw(self):
        raise AttributeError("cannot delete attribute")


# -- Pointer -----------------------------------------------------------------

_pointer_type_cache = {}


class _Pointer(_CData, metaclass=PyCPointerType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args):
        if len(args) > 1:
            raise TypeError("POINTER takes at most 1 argument")
        if args and args[0] is not None:
            self.contents = args[0]

    def _target_addr(self):
        return int.from_bytes(self._read(0, _PTR), _BO)

    @property
    def contents(self):
        addr = self._target_addr()
        if addr == 0:
            raise ValueError("NULL pointer access")
        tgt = _info_req(type(self)).proto
        if tgt is None:
            raise TypeError("Cannot dereference pointer to incomplete type")
        view = tgt.from_address(addr)
        object.__setattr__(view, "_b_base_", self)
        return view

    @contents.setter
    def contents(self, value):
        tgt = _info_req(type(self)).proto
        if not isinstance(value, _CData):
            raise TypeError(
                "expected %s instead of %s"
                % (tgt.__name__ if tgt else "ctypes instance",
                   type(value).__name__))
        if tgt is not None and not isinstance(value, tgt):
            raise TypeError(
                "expected %s instead of %s"
                % (tgt.__name__, type(value).__name__))
        self._write(0, addressof(value).to_bytes(_PTR, _BO))
        # CPython Pointer_set_contents: keep the object itself under
        # index 1, then whatever it keeps alive under index 0.
        _keep_ref(self, 1, value)
        _keep_ref(self, 0, _get_keeped(value))

    def _item_range(self, index):
        if not isinstance(index, slice):
            return None
        step = 1 if index.step is None else _index(index.step)
        if step == 0:
            raise ValueError("slice step cannot be zero")
        if index.start is None:
            if step < 0:
                raise ValueError("slice start is required for step < 0")
            start = 0
        else:
            start = _index(index.start)
        if index.stop is None:
            raise ValueError("slice stop is required")
        stop = _index(index.stop)
        return range(start, stop, step)

    def __getitem__(self, index):
        info = _info_req(type(self))
        tgt = info.proto
        einfo = _info_req(tgt)
        esize = einfo.size
        base = self._target_addr()
        if base == 0:
            raise ValueError("NULL pointer access")
        rng = self._item_range(index)
        if rng is not None:
            if einfo.code == "c" and _is_direct_simple(tgt):
                return bytes(
                    _nat.read_mem(base + i * esize, 1)[0] for i in rng)
            if einfo.code == "u" and _is_direct_simple(tgt):
                return "".join(self[i] for i in rng)
            return [self[i] for i in rng]
        index = _index(index)
        if einfo.code is not None and _is_direct_simple(tgt):
            tmp = tgt.from_address(base + index * esize)
            return _simple_get(einfo.code, tmp, 0, einfo.swapped)
        view = tgt.from_address(base + index * esize)
        object.__setattr__(view, "_b_base_", self)
        object.__setattr__(view, "_b_index", index)
        return view

    def __setitem__(self, index, value):
        info = _info_req(type(self))
        tgt = info.proto
        einfo = _info_req(tgt)
        esize = einfo.size
        base = self._target_addr()
        if base == 0:
            raise ValueError("NULL pointer access")
        if isinstance(index, slice):
            rng = self._item_range(index)
            for i, v in zip(rng, value):
                self[i] = v
            return
        index = _index(index)
        dst = tgt.from_address(base + index * esize)
        if einfo.code is not None and _is_direct_simple(tgt):
            keep, shadow = _simple_set(einfo.code, dst, value, 0,
                                       einfo.swapped)
            _keep_ref(self, index, keep, shadow)
        elif isinstance(value, _CData):
            _write_at(dst, 0, value._read(0, esize))
            _keep_ref(self, index, _get_keeped(value))
        else:
            tmp = tgt(value)
            _write_at(dst, 0, tmp._read(0, esize))
            _keep_ref(self, index, _get_keeped(tmp))

    def __bool__(self):
        return self._target_addr() != 0


def POINTER(cls):
    try:
        return _pointer_type_cache[cls]
    except (KeyError, TypeError):
        pass
    if isinstance(cls, str):
        # Incomplete forward reference: `POINTER("cell")`. Cached under the
        # id() of the created type so ctypes.SetPointerType can find it.
        ptr = PyCPointerType("LP_%s" % cls, (_Pointer,), {})
        _pointer_type_cache[id(ptr)] = ptr
        return ptr
    if cls is None:
        ptr = PyCPointerType("LP_None", (_Pointer,), {})
        _pointer_type_cache[cls] = ptr
        return ptr
    if not isinstance(cls, type):
        raise TypeError("must be a ctypes type")
    ptr = PyCPointerType("LP_%s" % cls.__name__, (_Pointer,),
                         {"_type_": cls})
    _pointer_type_cache[cls] = ptr
    return ptr


def pointer(obj):
    if not isinstance(obj, _CData):
        raise TypeError(
            "_type_ must have storage info")
    ptr_type = POINTER(type(obj))
    p = ptr_type()
    p.contents = obj
    return p


# -- CFuncPtr ----------------------------------------------------------------


class CFuncPtr(_CData, metaclass=PyCFuncPtrType):
    __slots__ = ("_handle_addr", "_callable", "_com_name",
                 "_i_restype", "_i_argtypes", "_i_errcheck", "_b_thunk")
    _b_root_ = True
    _argtypes_ = None
    _restype_ = None
    _flags_ = FUNCFLAG_CDECL

    # `restype`/`argtypes`/`errcheck` are per-instance configuration on a
    # foreign function; they shadow the class defaults (mirroring CPython's
    # getset descriptors over the C-level slots).
    @property
    def restype(self):
        try:
            return self._i_restype
        except AttributeError:
            return type(self)._restype_

    @restype.setter
    def restype(self, value):
        if value is not None and not isinstance(value, _CDataMeta) \
                and not callable(value):
            raise TypeError("restype must be a type, a callable, or None")
        self._i_restype = value

    @restype.deleter
    def restype(self):
        try:
            del self._i_restype
        except AttributeError:
            pass

    @property
    def argtypes(self):
        try:
            return self._i_argtypes
        except AttributeError:
            return type(self)._argtypes_

    @argtypes.setter
    def argtypes(self, value):
        if value is None:
            self._i_argtypes = None
            return
        value = tuple(value)
        for i, at in enumerate(value):
            if not hasattr(at, "from_param"):
                raise TypeError(
                    "item %d in _argtypes_ has no from_param method"
                    % (i + 1,))
        self._i_argtypes = value

    @argtypes.deleter
    def argtypes(self):
        try:
            del self._i_argtypes
        except AttributeError:
            pass

    @property
    def errcheck(self):
        try:
            return self._i_errcheck
        except AttributeError:
            return None

    @errcheck.setter
    def errcheck(self, value):
        if value is not None and not callable(value):
            raise TypeError("the errcheck attribute must be callable")
        self._i_errcheck = value

    @errcheck.deleter
    def errcheck(self):
        try:
            del self._i_errcheck
        except AttributeError:
            pass

    def __init__(self, *args):
        object.__setattr__(self, "_handle_addr", 0)
        object.__setattr__(self, "_callable", None)
        object.__setattr__(self, "_com_name", None)
        if not args:
            return
        if len(args) > 1 and not isinstance(args[0], tuple):
            raise TypeError("argument must be callable or integer function"
                            " address")
        arg = args[0]
        if isinstance(arg, int):
            self._set_address(arg)
        elif isinstance(arg, tuple):
            name_or_ord, dll = arg
            addr = _resolve_dll_symbol(dll, name_or_ord)
            self._set_address(addr)
            if not isinstance(name_or_ord, int):
                object.__setattr__(self, "_com_name", name_or_ord)
        elif callable(arg):
            object.__setattr__(self, "_callable", arg)
            closure_addr = _make_closure(self, arg)
            self._set_address(closure_addr)
        else:
            raise TypeError(
                "argument must be callable or integer function address"
            )

    def _set_address(self, addr):
        object.__setattr__(self, "_handle_addr", int(addr))
        self._write(0, (int(addr) & ((1 << (8 * _PTR)) - 1))
                    .to_bytes(_PTR, _BO))

    def _handle(self):
        try:
            return self._handle_addr
        except AttributeError:
            # Materialized straight from memory (struct/array field view,
            # from_buffer, pointer deref) — ``__init__`` never ran. CPython
            # reads the function pointer out of the instance buffer on
            # every call (PyCFuncPtr_call: ``*(void **)self->b_ptr``), so
            # serve it live: numpy's ``_resolve_dtypes_and_context`` hands
            # back a capsule-wrapped struct whose ``strided_loop`` field is
            # exactly such a view (RFC 0075 WS8).
            return int.from_bytes(self._read(0, _PTR), _BO)

    def __bool__(self):
        return self._handle() != 0 or getattr(self, "_callable", None) is not None

    def __call__(self, *args):
        handle = self._handle()
        argtypes = self.argtypes
        if len(args) > CTYPES_MAX_ARGCOUNT:
            raise ArgumentError(
                "too many arguments (%d), maximum is %d"
                % (len(args), CTYPES_MAX_ARGCOUNT))
        thunk = _INTERNAL_THUNKS.get(handle)
        if thunk is not None:
            # CPython's internal helpers (cast, string_at, memmove...) are
            # real C functions reached through PYFUNCTYPE prototypes; their
            # implementations receive the raw Python objects. Skip argtype
            # conversion so e.g. cast()'s `py_object` args stay unwrapped.
            return thunk(*args)
        _callable = getattr(self, "_callable", None)
        if _callable is not None and handle == 0:
            return _callable(*args)
        restype = self.restype
        flags = type(self)._flags_
        result = _ffi_invoke(handle, restype, argtypes, flags, args)
        errcheck = self.errcheck
        if errcheck is not None:
            result = errcheck(result, self, args)
        return result


# ---------------------------------------------------------------------------
# Public helpers
# ---------------------------------------------------------------------------


class _CArgObject:
    """CPython's PyCArgObject: the pass-by-reference / converted-parameter
    wrapper produced by ``byref()`` and the ``from_param`` methods."""

    __slots__ = ("tag", "_obj", "_value", "_shadow", "_offset")

    def __init__(self, *args, **kwargs):
        raise TypeError("cannot create '_CArgObject' instances")

    def _address(self):
        if isinstance(self._obj, _CData):
            return addressof(self._obj) + self._offset
        if self._shadow is not None:
            return _nat.addressof_buffer(self._shadow)
        if isinstance(self._value, int):
            return self._value
        raise TypeError("cannot convert to an address")

    def __repr__(self):
        try:
            shown = self._value if self._value is not None else self._obj
            return "<cparam '%s' (%r)>" % (self.tag, shown)
        except Exception:
            return "<cparam '%s'>" % (self.tag,)


def _new_parg(tag, obj, value, offset=0):
    parg = object.__new__(_CArgObject)
    object.__setattr__(parg, "tag", tag)
    object.__setattr__(parg, "_obj", obj)
    object.__setattr__(parg, "_value", value)
    object.__setattr__(parg, "_shadow", None)
    object.__setattr__(parg, "_offset", offset)
    return parg


def byref(obj, offset=0):
    if not isinstance(obj, _CData):
        raise TypeError("byref() argument must be a ctypes instance, not '%s'"
                        % type(obj).__name__)
    return _new_parg("P", obj, None, offset)


def sizeof(type_or_obj):
    if isinstance(type_or_obj, _CDataMeta):
        return _info_req(type_or_obj).size
    if isinstance(type_or_obj, _CData):
        return type_or_obj._b_size
    raise TypeError("this type has no size")


def alignment(type_or_obj):
    if isinstance(type_or_obj, _CDataMeta):
        return _info_req(type_or_obj).align
    if isinstance(type_or_obj, _CData):
        return _info_req(type(type_or_obj)).align
    raise TypeError("no alignment info")


def addressof(obj):
    if not isinstance(obj, _CData):
        raise TypeError("invalid type")
    return obj._addr()


def resize(obj, size):
    if not isinstance(obj, _CData):
        raise TypeError("expected ctypes instance")
    min_size = _info_req(type(obj)).size
    if size < min_size:
        raise ValueError("minimum size is %d" % min_size)
    if not isinstance(obj._b_buffer, bytearray) or obj._b_base_ is not None:
        raise ValueError(
            "Memory cannot be resized because this object doesn't own it")
    cur = obj._b_buffer
    if size > len(cur):
        cur.extend(b"\x00" * (size - len(cur)))
    object.__setattr__(obj, "_b_size", size)


def _resolve_dll_symbol(dll, name_or_ord):
    if isinstance(name_or_ord, int):
        raise TypeError("ordinal lookup is only supported on Windows")
    handle = dll._handle
    try:
        addr = _nat.dlsym(handle, name_or_ord)
    except OSError:
        addr = 0
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


def _parg_int(x):
    if isinstance(x, _CArgObject):
        if isinstance(x._value, int):
            return x._value
        return x._address()
    if isinstance(x, _SimpleCData):
        return _index(x.value)
    return _index(x)


def _parg_addr(x):
    if isinstance(x, _CArgObject):
        return x._address()
    return _addr_of(x)


# A size this large cannot be a real allocation request; CPython fails the
# same calls inside PyBytes_FromStringAndSize with MemoryError.
_ABSURD_SIZE = 1 << 47


def _thunk_memmove(dst, src, count):
    return _nat.memmove(_parg_addr(dst), _parg_addr(src), _parg_int(count))


def _thunk_memset(dst, c, count):
    return _nat.memset(_parg_addr(dst), _parg_int(c), _parg_int(count))


def _thunk_string_at(ptr, size=-1):
    size = _parg_int(size)
    if size >= _ABSURD_SIZE:
        raise MemoryError
    return _nat.string_at(_parg_addr(ptr), size)


def _thunk_wstring_at(ptr, size=-1):
    size = _parg_int(size)
    if size >= _ABSURD_SIZE:
        raise MemoryError
    return _nat.wstring_at(_parg_addr(ptr), size)


def _thunk_cast(ptr, obj, typ):
    """CPython ``cast(obj, typ)`` (Modules/_ctypes/callproc.c).

    Creates a *new* instance of pointer type ``typ`` whose **value** is the
    address ``obj`` converts to under ``c_void_p`` argument conversion.
    The source object's keepalive dict is *shared* with the result (and the
    source itself is retained in it under ``id(obj)``), exactly like
    CPython's ``cast_check_pointertype``/``cast`` pair.
    """
    if not (isinstance(typ, _CDataMeta)
            and (issubclass(typ, (_Pointer, CFuncPtr))
                 or (issubclass(typ, _SimpleCData)
                     and _info_req(typ).code in ("P", "z", "Z", "O")))):
        raise TypeError(
            "cast() argument 2 must be a pointer type, not %s"
            % getattr(typ, "__name__", type(typ).__name__))
    addr = _parg_addr(ptr)
    if issubclass(typ, CFuncPtr):
        result = typ(int(addr))
    else:
        result = _alloc_instance(typ)
        result._write(0, (int(addr) & ((1 << (8 * _PTR)) - 1))
                      .to_bytes(_PTR, _BO))
    if isinstance(obj, _CData):
        root = _container_of(obj)
        object.__setattr__(result, "_objects", root._objects)
        object.__setattr__(result, "_b_shadow", root._b_shadow)
        if isinstance(result._objects, dict):
            result._objects[id(obj)] = obj
    elif isinstance(obj, (bytes, bytearray)):
        shadow = bytearray(obj)
        shadow.append(0)
        _keep_ref(result, 0, obj, shadow)
    return result


_memmove_addr = _register_thunk(_thunk_memmove)
_memset_addr = _register_thunk(_thunk_memset)
_string_at_addr = _register_thunk(_thunk_string_at)
_wstring_at_addr = _register_thunk(_thunk_wstring_at)
_cast_addr = _register_thunk(_thunk_cast)


# ---------------------------------------------------------------------------
# Foreign function invocation + callbacks (native FFI bridge)
# ---------------------------------------------------------------------------


def _type_code_for_ffi(t):
    """Map a ctypes type (or None) to the format code the native FFI
    bridge understands."""
    if t is None:
        return None  # void
    if isinstance(t, _CDataMeta):
        if issubclass(t, _SimpleCData):
            code = _info_req(t).code
            return "h" if code == "v" else code
        if issubclass(t, (_Pointer, Array, Structure, Union, CFuncPtr)):
            return "P"
    if callable(t):
        return "i"  # `restype` as a callable: raw int result, then call it
    raise TypeError("unsupported ctypes type in FFI signature: %r" % (t,))


def _arg_to_ffi(value):
    """Marshal an argument with no declared argtype (CPython's
    ConvParam defaults)."""
    if isinstance(value, _CArgObject):
        if value.tag in ("P", "z", "Z"):
            return ("P", value._address())
        return (value.tag, value._value)
    if isinstance(value, (Array, Structure, Union)):
        return ("P", addressof(value))
    if isinstance(value, (_Pointer, CFuncPtr)):
        return ("P", int.from_bytes(value._read(0, _PTR), _BO))
    if isinstance(value, _SimpleCData):
        code = _info_req(type(value)).code
        if code in ("z", "Z", "P"):
            return ("P", int.from_bytes(value._read(0, _PTR), _BO))
        return ("h" if code == "v" else code, value.value)
    if value is None:
        return ("P", 0)
    if isinstance(value, bool):
        return ("i", int(value))
    if isinstance(value, int):
        return ("q", value)
    if isinstance(value, float):
        return ("d", value)
    if isinstance(value, bytes):
        return ("z", value)
    if isinstance(value, str):
        return ("Z", value)
    param = getattr(value, "_as_parameter_", None)
    if param is not None:
        return _arg_to_ffi(param)
    raise TypeError("cannot pass %r to a foreign function"
                    % (type(value).__name__,))


def _convert_args(argtypes, args):
    """Run the declared argtypes' from_param over the fixed arguments,
    raising ctypes.ArgumentError with CPython's shape on failure."""
    if not argtypes:
        return list(args)
    if len(args) < len(argtypes):
        raise TypeError(
            "this function takes at least %d argument%s (%d given)"
            % (len(argtypes), "" if len(argtypes) == 1 else "s", len(args))
        )
    conv = []
    for i, (at, val) in enumerate(zip(argtypes, args)):
        try:
            conv.append(at.from_param(val))
        except (TypeError, ValueError) as exc:
            raise ArgumentError(
                "argument %d: %s: %s" % (i + 1, type(exc).__name__, exc))
    conv.extend(args[len(argtypes):])
    return conv


def _ffi_invoke(addr, restype, argtypes, flags, args):
    if addr == 0:
        raise ValueError("attempt to call NULL function pointer")
    conv = _convert_args(argtypes, args)
    codes = []
    payloads = []
    n_declared = len(argtypes) if argtypes else 0
    for i, val in enumerate(conv):
        if i < n_declared:
            code = _type_code_for_ffi(argtypes[i])
            payload = _coerce_payload(code, val)
        else:
            code, payload = _arg_to_ffi(val)
        codes.append(code)
        payloads.append(payload)
    rcode = _type_code_for_ffi(restype)
    # Args past the declared argtypes form a variadic tail; the native
    # bridge needs the split point because Apple arm64 passes anonymous
    # args on the stack rather than in registers.
    n_fixed = n_declared if argtypes else len(conv)
    raw = _nat.call_function(addr, rcode, codes, payloads, int(flags), n_fixed)
    if restype is None:
        return None
    result = _wrap_result(restype, raw)
    # CPython's GetResult (callproc.c): a restype carrying _check_retval_
    # (ctypes.HRESULT -> _check_HRESULT) has the converted result passed
    # through the checker, whose return value replaces it — this is how
    # OleDLL turns FAILED HRESULTs into exceptions, before errcheck runs.
    checker = getattr(restype, "_check_retval_", None)
    if checker is not None:
        result = checker(result)
    return result


def _coerce_payload(code, value):
    if code == "O":
        # `py_object.from_param` wraps the live object in a cparam
        # (RFC 0060): unwrap it so the callee receives the object
        # itself, not the marshalling wrapper.
        if isinstance(value, _CArgObject):
            return value._value
        if isinstance(value, _SimpleCData):
            return value.value
        return value
    if isinstance(value, _CArgObject):
        if code in ("P", "z", "Z"):
            return value._address()
        return value._value
    if code in ("P", "z", "Z"):
        if isinstance(value, _CData):
            # Aggregates pass their own address; pointer-like scalars pass
            # the address they *hold*.
            if isinstance(value, (Structure, Union, Array)):
                return addressof(value)
            return int.from_bytes(value._read(0, _PTR), _BO)
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
    if isinstance(restype, _CDataMeta) and issubclass(restype, _SimpleCData):
        info = _info_req(restype)
        if info.code == "O":
            # py_object restype: the native bridge already converted the
            # returned PyObject* into the live object.
            return raw
        if _is_direct_simple(restype):
            obj = restype()
            obj.value = raw
            return obj.value
        obj = restype()
        obj.value = raw
        return obj
    if isinstance(restype, _CDataMeta) and issubclass(restype, _Pointer):
        p = restype()
        p._write(0, (int(raw) & ((1 << (8 * _PTR)) - 1)).to_bytes(_PTR, _BO))
        return p
    if isinstance(restype, _CDataMeta) and issubclass(restype, CFuncPtr):
        return restype(int(raw))
    if not isinstance(restype, _CDataMeta) and callable(restype):
        return restype(raw)
    return raw


def _from_closure_arg(argtype, raw):
    """Rebuild the declared ctypes argument a Python callback expects from
    the primitive the native trampoline delivers (CPython's getfunc /
    _ctypes_simple_instance split)."""
    if argtype is None or not isinstance(argtype, _CDataMeta):
        return raw
    if issubclass(argtype, _Pointer):
        p = _alloc_instance(argtype)
        p._write(0, (int(raw) & ((1 << (8 * _PTR)) - 1)).to_bytes(_PTR, _BO))
        return p
    if issubclass(argtype, _SimpleCData):
        info = _info_req(argtype)
        if not _is_direct_simple(argtype):
            # Simple *subclass*: the callback receives a live instance.
            inst = _alloc_instance(argtype)
            keep, shadow = _simple_set(info.code, inst, raw, 0, info.swapped)
            _keep_ref(inst, 0, keep, shadow)
            return inst
        if info.code == "c" and isinstance(raw, int):
            return bytes([raw & 0xFF])
        if info.code == "u" and isinstance(raw, int):
            return chr(raw)
        if info.code == "?":
            return bool(raw)
        return raw
    if issubclass(argtype, (Structure, Union)):
        # By-reference aggregates: the trampoline delivers the address.
        inst = _alloc_instance(argtype)
        _write_at(inst, 0, _nat.read_mem(int(raw), _info_req(argtype).size))
        return inst
    return raw


def _to_closure_result(restype, result):
    """Reduce a callback's Python return value to the primitive the native
    trampoline writes back, validating via the restype's setfunc."""
    if restype is None:
        return None
    if isinstance(restype, _CDataMeta) and issubclass(restype, _SimpleCData):
        tmp = _alloc_instance(restype)
        info = _info_req(restype)
        if isinstance(result, _SimpleCData):
            result = result.value
        _simple_set(info.code, tmp, result, 0, False)
        raw = _simple_get(info.code, tmp, 0, False)
        if info.code in ("z", "Z", "P"):
            return int.from_bytes(tmp._read(0, _PTR), _BO)
        if info.code == "c":
            return raw[0] if isinstance(raw, (bytes, bytearray)) else raw
        if info.code == "u":
            return ord(raw)
        return raw
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
    # (a) rebuilds each declared argtype from the raw primitive before the
    # call and (b) reduces the return value back to a primitive afterwards.
    # Exceptions are routed to sys.unraisablehook with CPython's exact
    # message shapes (Modules/_ctypes/callbacks.c).
    functype = type(funcptr)
    argtypes = tuple(functype._argtypes_ or ())
    restype = functype._restype_
    argcodes = [_type_code_for_ffi(t) for t in argtypes]
    rcode = _type_code_for_ffi(restype) if restype is not None else None

    def _closure_entry(*raw):
        try:
            conv = [_from_closure_arg(at, val)
                    for at, val in zip(argtypes, raw)]
            if len(raw) > len(argtypes):
                conv.extend(raw[len(argtypes):])
            result = callable_(*conv)
        except BaseException as exc:
            _nat.unraisable(
                exc,
                "Exception ignored on calling ctypes callback function %r"
                % (callable_,))
            return 0
        try:
            return _to_closure_result(restype, result)
        except BaseException as exc:
            _nat.unraisable(
                exc,
                "Exception ignored on converting result of ctypes callback "
                "function %r" % (callable_,))
            return 0

    try:
        return _nat.create_closure(_closure_entry, rcode, argcodes)
    except NotImplementedError:
        return 0
