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

import builtins as _builtins
import sys as _sys
import _weakref
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

# paramflags direction bits (ctypes.h).
PARAMFLAG_FIN = 0x1
PARAMFLAG_FOUT = 0x2
PARAMFLAG_FLCID = 0x4

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
        "pointer_type", # cached POINTER(cls) (3.14 `__pointer_type__`)
        "ffi_descr",    # cached by-value aggregate descriptor for the bridge
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
        self.pointer_type = None
        self.ffi_descr = None


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


_MISSING = object()  # getattr sentinel: "attribute absent" vs. a None value


def _tp_full_name(tp):
    """CPython's %T: the fully qualified type name ("ctypes.c_char_p",
    "int" for builtins)."""
    mod = getattr(tp, "__module__", None)
    qn = getattr(tp, "__qualname__", tp.__name__)
    if mod is None or mod == "builtins":
        return qn
    return "%s.%s" % (mod, qn)


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

# The `_type_` codes a PyCSimpleType accepts (cfield.c SIMPLE_TYPE_CHARS);
# 'X' (BSTR) exists on Windows only. Listed in CPython's order so the
# "must be one of" message matches test_c_simple_type_meta.
_SIMPLE_TYPE_CHARS = "cbBhHiIlLdfuzZqQP" + ("X" if _sys.platform == "win32" else "") + "Ov?g"

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
        # cfield.c c_set (3.14 wording).
        if isinstance(value, (bytes, bytearray)):
            if len(value) != 1:
                raise TypeError(
                    "one character bytes, bytearray, or an integer in "
                    "range(256) expected, not %s of length %d"
                    % ("bytes" if isinstance(value, bytes) else "bytearray",
                       len(value)))
            b = bytes(value)
        elif isinstance(value, int):
            if not 0 <= value < 256:
                raise TypeError("integer not in range(256)")
            b = bytes([value])
        else:
            raise TypeError(
                "one character bytes, bytearray, or an integer in "
                "range(256) expected, not %s" % (_tp_full_name(type(value)),))
        _write_at(obj, off, b)
        return None, None
    if code == "?":
        _write_at(obj, off, b"\x01" if value else b"\x00")
        return None, None
    if code == "u":
        # cfield.c u_set (3.14 wording).
        if not isinstance(value, str):
            raise TypeError(
                "a unicode character expected, not instance of %s"
                % (_tp_full_name(type(value)),))
        if len(value) != 1:
            raise TypeError(
                "a unicode character expected, not a string of length %d"
                % (len(value),))
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
    """CPython 3.14 ``_ctypes.CField``: a struct/union field descriptor.

    ``byte_offset``/``byte_size`` describe the storage unit; a bitfield
    adds ``bit_size``/``bit_offset`` inside it. The legacy ``size`` is
    ``byte_size`` for plain fields and ``(bit_size << 16) | bit_offset``
    for bitfields (the packed value older code inspected).
    """
    __slots__ = ("name", "type", "byte_offset", "byte_size", "index",
                 "_bitfield_size", "_bit_offset", "_anonymous", "_swapped")
    __module__ = "ctypes"  # tp_name "ctypes.CField"
    __flags__ = _TPFLAGS_IMMUTABLETYPE

    def __new__(cls, *, name, type, byte_size, byte_offset, index,
                _internal_use=False, bit_size=None, bit_offset=None):
        if not _internal_use:
            # Do not instantiate outside ctypes, yet.
            raise TypeError("cannot create %s object" % (cls.__name__,))
        if not isinstance(name, str):
            raise TypeError(
                "argument 'name' must be str, not %s" % (
                    _builtins.type(name).__name__,))
        byte_size = _index(byte_size)
        byte_offset = _index(byte_offset)
        index = _index(index)
        if byte_size < 0:
            raise ValueError(
                "byte size of field %r must not be negative, got %d"
                % (name, byte_size))
        info = _info(type)
        if info is None:
            raise TypeError("type of field %r must be a C type" % (name,))
        if byte_size != info.size:
            raise ValueError(
                "byte size of field %r (%d) does not match type size (%d)"
                % (name, byte_size, info.size))
        bitfield_size = 0
        bitoff = 0
        if bit_size is not None:
            if not _bitfield_allowed(type, info):
                raise TypeError(
                    "bit fields not allowed for type %s" % (type.__name__,))
            if byte_size > 100:
                raise ValueError(
                    "bit field %r size too large, got %d" % (name, byte_size))
            bitfield_size = _index(bit_size)
            if bitfield_size <= 0 or bitfield_size > 255:
                raise ValueError(
                    "bit size of field %r out of range, got %d"
                    % (name, bitfield_size))
            bitoff = _index(bit_offset)
            if bitoff < 0 or bitoff > 255:
                raise ValueError(
                    "bit offset of field %r out of range, got %d"
                    % (name, bitoff))
            if bitfield_size + bitoff > byte_size * 8:
                raise ValueError(
                    "bit field %r overflows its type (%d + %d > %d)"
                    % (name, bitoff, bitfield_size, byte_size * 8))
        elif bit_offset is not None:
            raise ValueError(
                "field %r: bit_offset must be specified if bit_size is"
                % (name,))
        fld = object.__new__(cls)
        # PyUnicode_FromObject: a str subclass is copied to an exact str
        # (test_struct_fields.test_str_name).
        if _builtins.type(name) is not str:
            name = str.__str__(name)
        object.__setattr__(fld, "name", name)
        object.__setattr__(fld, "type", type)
        object.__setattr__(fld, "byte_offset", byte_offset)
        object.__setattr__(fld, "byte_size", byte_size)
        object.__setattr__(fld, "index", index)
        object.__setattr__(fld, "_bitfield_size", bitfield_size)
        object.__setattr__(fld, "_bit_offset", bitoff)
        object.__setattr__(fld, "_anonymous", False)
        object.__setattr__(fld, "_swapped", False)
        return fld

    def __init__(self, *args, **kwargs):
        pass

    def __setattr__(self, name, value):
        raise AttributeError(
            "readonly attribute" if name in CField.__slots__
            else "'CField' object has no attribute '%s'" % (name,))

    # -- 3.14 attribute surface ------------------------------------------

    @property
    def offset(self):
        """offset in bytes of this field (same as byte_offset)"""
        return self.byte_offset

    @property
    def size(self):
        """size in bytes of this field. For bitfields, this is a legacy
        packed value; use byte_size instead"""
        if self._bitfield_size:
            return (self._bitfield_size << 16) | self._bit_offset
        return self.byte_size

    @property
    def bit_size(self):
        """size of this field in bits"""
        if self._bitfield_size:
            return self._bitfield_size
        return self.byte_size * 8

    @property
    def bit_offset(self):
        """additional offset in bits (relative to byte_offset); zero for
        non-bitfields"""
        return self._bit_offset

    @property
    def is_bitfield(self):
        """true if this is a bitfield"""
        return bool(self._bitfield_size)

    @property
    def is_anonymous(self):
        """true if this field is anonymous"""
        return self._anonymous

    def __repr__(self):
        # cfield.c uses %T: the fully qualified type name.
        tp = _builtins.type(self)
        mod = tp.__module__
        tname = tp.__qualname__ if mod == "builtins" else (
            "%s.%s" % (mod, tp.__qualname__))
        if self._bitfield_size:
            return "<%s %r type=%s, ofs=%d, bit_size=%d, bit_offset=%d>" % (
                tname, self.name, self.type.__name__,
                self.byte_offset, self._bitfield_size, self._bit_offset)
        return "<%s %r type=%s, ofs=%d, size=%d>" % (
            tname, self.name, self.type.__name__,
            self.byte_offset, self.byte_size)

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        if not isinstance(obj, _CData):
            raise TypeError("not a ctype instance")
        ftype = self.type
        finfo = _info_req(ftype)
        if self._bitfield_size:
            return self._get_bits(obj)
        if finfo.code is not None and _is_direct_simple(ftype):
            return _simple_get(finfo.code, obj, self.byte_offset, finfo.swapped)
        # Fields typed as c_char/c_wchar arrays read as bytes/str
        # (CPython installs s_get/U_get for them).
        akind = _char_array_kind(ftype)
        if akind == "c":
            data = _read_at(obj, self.byte_offset, finfo.size)
            nul = data.find(b"\x00")
            return data if nul < 0 else data[:nul]
        if akind == "u":
            return _wchar_decode(_read_at(obj, self.byte_offset, finfo.size))
        return _view(obj, ftype, self.byte_offset, self.index)

    def __set__(self, obj, value):
        if not isinstance(obj, _CData):
            raise TypeError("not a ctype instance")
        ftype = self.type
        finfo = _info_req(ftype)
        if self._bitfield_size:
            self._set_bits(obj, value)
            return
        if finfo.code is not None and _is_direct_simple(ftype) \
                and not isinstance(value, _CData):
            # _PyCData_set: a plain Python value goes through the type's
            # setfunc; a ctypes instance takes the generic path below
            # (memcpy for an instance of the field type, else the
            # "incompatible types" error).
            keep, shadow = _simple_set(
                finfo.code, obj, value, self.byte_offset, finfo.swapped)
            _keep_ref(obj, self.index, keep, shadow)
            return
        akind = _char_array_kind(ftype)
        if akind == "c":
            # CPython s_set: strlen-bounded copy plus NUL if it fits.
            if not isinstance(value, bytes):
                raise TypeError(
                    "expected bytes, %s found" % (_builtins.type(value).__name__,))
            nul = value.find(b"\x00")
            data = value if nul < 0 else value[:nul]
            if len(data) < finfo.size:
                data += b"\x00"
            elif len(data) > finfo.size:
                raise ValueError("byte string too long")
            _write_at(obj, self.byte_offset, data)
            return
        if akind == "u":
            if not isinstance(value, str):
                raise TypeError(
                    "unicode string expected instead of %s instance"
                    % (_builtins.type(value).__name__,))
            nchars = finfo.size // _WCHAR
            nul = value.find("\x00")
            data = value if nul < 0 else value[:nul]
            if len(data) > nchars:
                raise ValueError("string too long")
            raw = b"".join(ord(c).to_bytes(_WCHAR, _BO) for c in data)
            if len(data) < nchars:
                raw += (0).to_bytes(_WCHAR, _BO)
            _write_at(obj, self.byte_offset, raw)
            return
        _cdata_set(obj, ftype, self.byte_offset, self.index, value)

    def __delete__(self, obj):
        raise TypeError("can't delete attribute")

    # -- bitfields --------------------------------------------------------

    def _unit(self, obj):
        finfo = _info(self.type)
        size, signed = _INT_CODES[finfo.code]
        bo = _BO_SWAP if finfo.swapped else _BO
        v = int.from_bytes(_read_at(obj, self.byte_offset, size), bo)
        return v, size, signed, bo

    def _get_bits(self, obj):
        v, size, signed, _bo = self._unit(obj)
        v = (v >> self._bit_offset) & ((1 << self._bitfield_size) - 1)
        if signed and v >= (1 << (self._bitfield_size - 1)):
            v -= 1 << self._bitfield_size
        return v

    def _set_bits(self, obj, value):
        value = _index(value)
        v, size, _signed, bo = self._unit(obj)
        mask = (1 << self._bitfield_size) - 1
        v &= ~(mask << self._bit_offset)
        v |= (value & mask) << self._bit_offset
        _write_at(obj, self.byte_offset, v.to_bytes(size, bo))


def _bitfield_allowed(ftype, info):
    """CPython's ffi-type switch: integer units only (no c_char/c_wchar,
    no floats, pointers, or aggregates)."""
    code = info.code
    if code is None or code in ("c", "u"):
        return False
    return code in _INT_CODES


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

    # -- 3.14 `__pointer_type__` (ctype_get/set_pointer_type) --------------

    @property
    def __pointer_type__(cls):
        info = _info(cls)
        if info is None:
            raise TypeError("%r must have storage info" % (cls,))
        if info.pointer_type is not None:
            return info.pointer_type
        raise AttributeError(
            "%r has no attribute '__pointer_type__'" % (cls,))

    @__pointer_type__.setter
    def __pointer_type__(cls, value):
        info = _info(cls)
        if info is None:
            raise TypeError("%r must have storage info" % (cls,))
        info.pointer_type = value

    @__pointer_type__.deleter
    def __pointer_type__(cls):
        info = _info(cls)
        if info is None:
            raise TypeError("%r must have storage info" % (cls,))
        info.pointer_type = None

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
        if code not in _SIMPLE_TYPE_CHARS:
            raise AttributeError(
                "class must define a '_type_' attribute which must be\n"
                "a single character string containing one of '%s'."
                % (_SIMPLE_TYPE_CHARS,)
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
        except TypeError:
            # PyCSimpleType_from_param: `_as_parameter_` is evaluated
            # recursively (a self-referential one ends in RecursionError).
            param = getattr(value, "_as_parameter_", _MISSING)
            if param is _MISSING:
                raise
            return cls.from_param(param)


def _points_at_code(value, code):
    """c_char_p_from_param / c_wchar_p_from_param: an array or pointer whose
    element type has the given simple code, or a `byref()` cparam wrapping
    an instance of such a type, is accepted as the string pointer."""
    if isinstance(value, (_Pointer, Array)):
        it = _info_req(type(value))
        pinfo = _info(it.proto) if isinstance(it.proto, type) else None
        return pinfo is not None and pinfo.code == code
    if isinstance(value, _CArgObject):
        obj = value._obj
        if isinstance(obj, _CData):
            oinfo = _info(type(obj))
            return oinfo is not None and oinfo.code == code
    return False


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
        if _points_at_code(value, "c"):
            return value
        raise TypeError(
            "'%s' object cannot be interpreted as ctypes.c_char_p"
            % (type(value).__name__,))
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
        if _points_at_code(value, "u"):
            return value
        raise TypeError(
            "'%s' object cannot be interpreted as ctypes.c_wchar_p"
            % (type(value).__name__,))
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
        raise TypeError(
            "'%s' object cannot be interpreted as ctypes.c_void_p"
            % (type(value).__name__,))
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
        # PEP 3118 (PyCArrayType_init): `format` is the innermost element's
        # format; the dims live in `shape` and are prefixed as one
        # parenthesised group ("(2,3)<f" for c_float*3*2) by consumers.
        info.format = einfo.format if einfo.format is not None else "B"
        _set_info(cls, info)


def _pointer_set_proto(cls, info, proto):
    """CPython PyCPointerType_SetProto: record the target type and, if the
    target has no cached pointer type yet, make `cls` its
    `__pointer_type__`."""
    if not isinstance(proto, type):
        raise TypeError("_type_ must be a type")
    tinfo = _info(proto)
    if tinfo is None:
        raise TypeError("%r must have storage info" % (proto,))
    info.proto = proto
    if tinfo.pointer_type is None:
        tinfo.pointer_type = cls


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
        _set_info(cls, info)
        # PyCPointerType_init reads `_type_` from the class body only: a
        # subclass of POINTER(T) without its own `_type_` stays incomplete.
        proto = namespace.get("_type_")
        if proto is not None:
            _pointer_set_proto(cls, info, proto)
            # PEP 3118 pointer format is snapshotted at creation time (an
            # incomplete target that is completed later keeps "&B" -- see
            # the "not fixed" remark in test_pep3118). Creating a pointer
            # type does NOT finalize the target: `POINTER(Incomplete)`
            # before `_fields_` assignment is the forward-reference idiom.
            tinfo = _info(proto)
            fmt, _ndim, shape = buffer_info(proto)
            info.format = "&" + _shape_prefix(shape) + (fmt or "B")

    def set_type(cls, t):
        info = _info_req(cls)
        _pointer_set_proto(cls, info, t)
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
    if "_abstract_" in namespace:
        return  # kept for bw compatibility: no StgInfo => "abstract class"
    info = _StgInfo()
    binfo = _info(base) if isinstance(base, type) else None
    if binfo is not None:
        binfo.final = True  # subclassing finalizes the base (test_4)
        info.size = binfo.size
        info.align = binfo.align
        info.length = binfo.length
        info.fields = dict(binfo.fields) if binfo.fields else {}
        if "_fields_" not in namespace:
            info.format = binfo.format  # PyCStgInfo_clone
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
    """CPython PyCStructUnionType_update_stginfo: delegate the layout to
    ctypes._layout.get_layout (pure Python since 3.14) and install the
    CField descriptors it produced."""
    info = _info(cls)
    if info is None:
        raise TypeError("ctypes state is not initialized")
    if info.final:
        raise AttributeError("_fields_ is final")

    base = cls.__mro__[1] if len(cls.__mro__) > 1 else None
    binfo = _info(base) if isinstance(base, type) else None

    from ctypes._layout import get_layout
    layout = get_layout(cls, value, is_struct=not union,
                        base=base if binfo is not None else None)

    total_align = _index(layout.align)
    if total_align < 0:
        raise ValueError("align must be a non-negative integer")
    total_size = _index(layout.size)
    if total_size < 0:
        raise ValueError("size must be a non-negative integer")
    format_spec = layout.format_spec
    if not isinstance(format_spec, str):
        raise TypeError("format_spec must be str")
    layout_fields = tuple(layout.fields)

    fields = dict(binfo.fields) if binfo is not None and binfo.fields else {}
    base_len = binfo.length if binfo is not None else 0
    for i, prop in enumerate(layout_fields):
        if not isinstance(prop, CField):
            raise TypeError(
                "fields must be of type CField, got %s"
                % (type(prop).__name__,))
        if prop.index != i:
            raise ValueError(
                "field %r index mismatch (expected %d, got %d)"
                % (prop.name, i, prop.index))
        finfo = _info(prop.type)
        finfo.final = True  # using a type as a field finalizes it (test_3)
        fields[prop.name] = prop
        type.__setattr__(cls, prop.name, prop)

    if total_size > _sys.maxsize:
        raise OverflowError("structure or union is too large")

    # We did check that this flag was NOT set above; it must not have been
    # set until now (a field referenced the class being laid out).
    if info.final:
        raise AttributeError("Structure or union cannot contain itself")

    info.format = format_spec
    info.fields = fields
    info.size = total_size
    info.align = total_align
    info.length = base_len + len(layout_fields)
    info.final = True
    _make_anon_fields(cls, info)
    type.__setattr__(cls, "_fields_", value)


def _make_fields(cls, info, descr, index, offset):
    """CPython MakeFields: re-home every `_fields_` descriptor of the
    anonymous member `descr` onto `cls`, offsets and indices adjusted."""
    fieldlist = descr.type._fields_
    try:
        fieldlist = list(fieldlist)
    except TypeError:
        raise TypeError("_fields_ must be a sequence")
    for pair in fieldlist:
        fname = pair[0]
        fdescr = getattr(descr.type, fname)
        if type(fdescr) is not CField:
            raise TypeError("unexpected type")
        if fdescr.is_anonymous:
            _make_fields(cls, info, fdescr, index + fdescr.index,
                         offset + fdescr.byte_offset)
            continue
        new_descr = CField(
            name=fdescr.name,
            type=fdescr.type,
            byte_size=fdescr.byte_size,
            byte_offset=fdescr.byte_offset + offset,
            index=fdescr.index + index,
            _internal_use=True,
            bit_size=fdescr._bitfield_size if fdescr.is_bitfield else None,
            bit_offset=fdescr._bit_offset if fdescr.is_bitfield else None,
        )
        type.__setattr__(cls, fname, new_descr)


def _make_anon_fields(cls, info):
    """CPython MakeAnonFields."""
    anon = getattr(cls, "_anonymous_", None)
    if anon is None:
        return
    try:
        anon_names = list(anon)
    except TypeError:
        raise TypeError("_anonymous_ must be a sequence")
    for fname in anon_names:
        descr = getattr(cls, fname)
        if type(descr) is not CField:
            raise AttributeError(
                "'%s' is specified in _anonymous_ but not in _fields_"
                % (fname,))
        object.__setattr__(descr, "_anonymous", True)
        _make_fields(cls, info, descr, descr.index, descr.byte_offset)


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
    if finfo.code is not None and not isinstance(value, _CData):
        # A plain Python value assigned to a simple-type slot (including
        # subclasses of the simple types) goes through the type's setfunc.
        keep, shadow = _simple_set(finfo.code, dst, value, offset, finfo.swapped)
        _keep_ref(dst, index, keep, shadow)
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
    if isinstance(value, tuple) and not issubclass(ftype, _Pointer):
        # _PyCData_set: a tuple is passed to the type; a failure there is
        # re-raised as RuntimeError via _ctypes_extend_error.
        try:
            tmp = ftype(*value)
        except Exception as exc:
            raise RuntimeError(
                "(%s) %s: %s" % (ftype.__name__, type(exc).__name__, exc))
        _write_at(dst, offset, tmp._read(0, finfo.size))
        _keep_ref(dst, index, _get_keeped(tmp))
        return
    if not isinstance(value, _CData):
        raise TypeError(
            "expected %s instance, got %s"
            % (ftype.__name__, type(value).__name__))
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

    def __hash__(self):
        # PyCData_nohash: every ctypes instance is unhashable, and none of
        # them define value-based equality.
        raise TypeError("unhashable type")

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
        fmt, _ndim, dims = buffer_info(type(self))
        t = type(self)
        while isinstance(t, PyCArrayType):
            t = _info_req(t).proto
        ti = _info(t)
        itemsize = ti.size if ti is not None else 1
        _nat.configure_view(mv, fmt or "B", itemsize,
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


# -- Structure / Union -------------------------------------------------------


def _init_pos_args(self, tp, args, kw, index):
    """CPython _init_pos_args: assign positional initializers to the
    fields of `tp`, base classes first; returns the next field index."""
    base = tp.__mro__[1] if len(tp.__mro__) > 1 else None
    if base is not None and _info(base) is not None:
        index = _init_pos_args(self, base, args, kw, index)
    fields = tp.__dict__.get("_fields_")
    if fields is None:
        return index
    fields = list(fields)
    info = _info_req(tp)
    i = index
    while i < info.length and i < len(args):
        name = fields[i - index][0]
        if kw and name in kw:
            raise TypeError("duplicate values for field %r" % (name,))
        setattr(self, name, args[i])
        i += 1
    return info.length


def _struct_union_init(self, args, kw):
    """CPython Struct_init."""
    _info_req(type(self))
    if args:
        res = _init_pos_args(self, type(self), args, kw, 0)
        if res < len(args):
            raise TypeError("too many initializers")
    for key, val in kw.items():
        setattr(self, key, val)


class Structure(_CData, metaclass=PyCStructType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args, **kw):
        _struct_union_init(self, args, kw)


class Union(_CData, metaclass=UnionType):
    __slots__ = ()
    _b_root_ = True

    def __init__(self, *args, **kw):
        _struct_union_init(self, args, kw)


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


class _Pointer(_CData, metaclass=PyCPointerType):
    __slots__ = ()
    _b_root_ = True

    def __new__(cls, *args, **kw):
        # Pointer_new: an incomplete pointer type (no `_type_`, e.g. a
        # subclass of POINTER(T) or POINTER("name")) cannot be instantiated.
        info = _info(cls)
        if info is None or info.proto is None:
            raise TypeError("Cannot create instance: has no _type_")
        return _CData.__new__(cls, *args, **kw)

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
            # Pointers have sq_ass_item only (no mp_ass_subscript): slice
            # assignment falls through to the sequence protocol's check.
            raise TypeError("sequence index must be integer, not 'slice'")
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


# -- CFuncPtr ----------------------------------------------------------------


class CFuncPtr(_CData, metaclass=PyCFuncPtrType):
    __slots__ = ("_handle_addr", "_callable", "_com_name", "_paramflags",
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
        try:
            value = tuple(value)
        except TypeError:
            raise TypeError("_argtypes_ must be a sequence of types")
        if len(value) > CTYPES_MAX_ARGCOUNT:
            raise ArgumentError(
                "_argtypes_ has too many arguments (%d), maximum is %d"
                % (len(value), CTYPES_MAX_ARGCOUNT))
        for i, at in enumerate(value):
            if not hasattr(at, "from_param"):
                raise TypeError(
                    "item %d in _argtypes_ has no from_param method"
                    % (i + 1,))
        _validate_paramflags(getattr(self, "_paramflags", None), value)
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
        object.__setattr__(self, "_paramflags", None)
        if not args:
            return
        if len(args) > 1 and not isinstance(args[0], tuple):
            raise TypeError("argument must be callable or integer function"
                            " address")
        arg = args[0]
        if isinstance(arg, int):
            self._set_address(arg)
        elif isinstance(arg, tuple):
            # PyCFuncPtr_FromDll: (name_or_ordinal, dll)[, paramflags]
            if len(args) > 2:
                raise TypeError(
                    "function takes at most 2 arguments (%d given)"
                    % (len(args),))
            paramflags = args[1] if len(args) > 1 else None
            if len(arg) != 2:
                raise TypeError("illegal func_spec argument")
            name_or_ord, dll = arg
            addr = _resolve_dll_symbol(dll, name_or_ord)
            _validate_paramflags(paramflags, type(self)._argtypes_)
            self._set_address(addr)
            object.__setattr__(self, "_paramflags", paramflags)
            if not isinstance(name_or_ord, int):
                object.__setattr__(self, "_com_name", name_or_ord)
        elif callable(arg):
            # _ctypes_alloc_callback: only result types with a setfunc
            # (the fundamental simple types) may be returned by a callback.
            restype = type(self)._restype_
            if restype is not None:
                rinfo = _info(restype) if isinstance(restype, type) else None
                if rinfo is None or rinfo.code is None:
                    raise TypeError(
                        "invalid result type for callback function")
            # PyCFuncPtr_new: the thunk owns the callable and the native
            # closure; it is kept alive through `_objects['0']`, so a
            # struct field holding this function pointer keeps the thunk
            # (and therefore the callback) alive along with it.
            thunk = _make_closure(type(self), arg)
            object.__setattr__(self, "_callable", arg)
            _keep_ref(self, 0, thunk)
            self._set_address(thunk._closure)
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

    def __call__(self, *args, **kwds):
        handle = self._handle()
        argtypes = self.argtypes
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
        paramflags = getattr(self, "_paramflags", None)
        callargs, outmask, inoutmask, numretvals = _build_callargs(
            paramflags, argtypes, args, kwds)
        if len(callargs) > CTYPES_MAX_ARGCOUNT:
            raise ArgumentError(
                "too many arguments (%d), maximum is %d"
                % (len(callargs), CTYPES_MAX_ARGCOUNT))
        restype = self.restype
        flags = type(self)._flags_
        if argtypes:
            required, actual = len(argtypes), len(callargs)
            if (flags & FUNCFLAG_CDECL) == FUNCFLAG_CDECL:
                if required > actual:
                    raise TypeError(
                        "this function takes at least %d argument%s (%d given)"
                        % (required, "" if required == 1 else "s", actual))
            elif required != actual:
                raise TypeError(
                    "this function takes %d argument%s (%d given)"
                    % (required, "" if required == 1 else "s", actual))
        result = _ffi_invoke(handle, restype, argtypes, flags, callargs)
        errcheck = self.errcheck
        if errcheck is not None:
            result = errcheck(result, self, callargs)
        return _build_result(result, callargs, outmask, inoutmask, numretvals)


def _check_outarg_type(tp, index):
    """CPython _check_outarg_type: an 'out' parameter must be a pointer,
    an array, or one of the simple pointer types (c_void_p & friends)."""
    if isinstance(tp, (PyCPointerType, PyCArrayType)):
        return
    info = _info(tp) if isinstance(tp, type) else None
    if info is not None and info.code in ("P", "z", "Z"):
        return
    raise TypeError(
        "'out' parameter %d must be a pointer type, not %s"
        % (index, tp.__name__ if isinstance(tp, type)
           else type(tp).__name__))


def _validate_paramflags(paramflags, argtypes):
    """CPython _validate_paramflags."""
    if paramflags is None or argtypes is None:
        return
    if not isinstance(paramflags, tuple):
        raise TypeError("paramflags must be a tuple or None")
    if len(paramflags) != len(argtypes):
        raise ValueError(
            "paramflags must have the same length as argtypes")
    for i, item in enumerate(paramflags):
        ok = isinstance(item, tuple) and 1 <= len(item) <= 3 \
            and isinstance(item[0], int) and not isinstance(item[0], bool) \
            and (len(item) < 2 or item[1] is None or isinstance(item[1], str))
        if not ok:
            raise TypeError(
                "paramflags must be a sequence of "
                "(int [,string [,value]]) tuples")
        flag = item[0] & (PARAMFLAG_FIN | PARAMFLAG_FOUT | PARAMFLAG_FLCID)
        if flag in (0, PARAMFLAG_FIN, PARAMFLAG_FIN | PARAMFLAG_FLCID,
                    PARAMFLAG_FIN | PARAMFLAG_FOUT):
            continue
        if flag == PARAMFLAG_FOUT:
            _check_outarg_type(argtypes[i], i + 1)
            continue
        raise TypeError("paramflag value %d not supported" % (item[0],))


_NO_DEFAULT = object()


def _get_arg(state, name, defval, inargs, kwds):
    """CPython _get_arg: `state` is a one-element list holding the
    running inargs index."""
    if state[0] < len(inargs):
        v = inargs[state[0]]
        state[0] += 1
        return v
    if kwds and name is not None and name in kwds:
        state[0] += 1
        return kwds[name]
    if defval is not _NO_DEFAULT:
        return defval
    if name is not None:
        raise TypeError("required argument '%s' missing" % (name,))
    raise TypeError("not enough arguments")


def _build_callargs(paramflags, argtypes, inargs, kwds):
    """CPython _build_callargs: returns (callargs, outmask, inoutmask,
    numretvals)."""
    if not argtypes or paramflags is None:
        return tuple(inargs), 0, 0, 0
    n = len(argtypes)
    callargs = [None] * n
    outmask = inoutmask = 0
    numretvals = 0
    state = [0]
    for i in range(n):
        item = paramflags[i]
        flag = item[0] & 0xFFFFFFFF
        name = item[1] if len(item) > 1 else None
        defval = item[2] if len(item) > 2 else _NO_DEFAULT
        kind = flag & (PARAMFLAG_FIN | PARAMFLAG_FOUT | PARAMFLAG_FLCID)
        if kind == PARAMFLAG_FIN | PARAMFLAG_FLCID:
            callargs[i] = 0 if defval is _NO_DEFAULT else defval
        elif kind in (0, PARAMFLAG_FIN, PARAMFLAG_FIN | PARAMFLAG_FOUT):
            if kind == PARAMFLAG_FIN | PARAMFLAG_FOUT:
                inoutmask |= 1 << i
                numretvals += 1
            callargs[i] = _get_arg(state, name, defval, inargs, kwds)
        elif kind == PARAMFLAG_FOUT:
            if defval is not _NO_DEFAULT:
                callargs[i] = defval
            else:
                tp = argtypes[i]
                info = _info(tp)
                if info is None:
                    raise RuntimeError("NULL stginfo unexpected")
                if isinstance(info.proto, str):
                    raise TypeError(
                        "%s 'out' parameter must be passed as default value"
                        % (tp.__name__,))
                if isinstance(tp, PyCArrayType):
                    callargs[i] = tp()
                else:
                    callargs[i] = info.proto()
            outmask |= 1 << i
            numretvals += 1
        else:
            raise ValueError("paramflag %d not yet implemented" % (flag,))
    actual = len(inargs) + (len(kwds) if kwds else 0)
    if actual != state[0]:
        raise TypeError("call takes exactly %d arguments (%d given)"
                        % (state[0], actual))
    return tuple(callargs), outmask, inoutmask, numretvals


def _build_result(result, callargs, outmask, inoutmask, numretvals):
    """CPython _build_result."""
    if numretvals == 0:
        return result
    out = []
    for i in range(32):
        bit = 1 << i
        if bit & inoutmask:
            out.append(callargs[i])
        elif bit & outmask:
            out.append(callargs[i].__ctypes_from_outparam__())
        if len(out) == numretvals:
            break
    if numretvals == 1:
        return out[0]
    return tuple(out)


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
        # callproc.c PyCArg_repr.
        tag = self.tag
        if tag in "bBhHiIlLqQ":
            return "<cparam '%s' (%d)>" % (tag, self._value)
        if tag in "df":
            return "<cparam '%s' (%r)>" % (tag, float(self._value))
        if tag == "c":
            v = self._value
            ch = v[0] if isinstance(v, (bytes, bytearray)) else int(v) & 0xFF
            if 32 <= ch < 127 and ch not in (0x27, 0x5c):
                return "<cparam 'c' ('%s')>" % (chr(ch),)
            return "<cparam 'c' ('\\x%02x')>" % (ch,)
        if tag in "zZP":
            try:
                addr = self._address()
            except TypeError:
                addr = 0
            return "<cparam '%s' (0x%016x)>" % (tag, addr)
        return "<cparam '%s' at 0x%012x>" % (tag, id(self))


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


def _shape_prefix(shape):
    """"(2,3)" for an array shape, "" for scalars
    (_ctypes_alloc_format_string_with_shape)."""
    if not shape:
        return ""
    return "(" + ",".join(str(d) for d in shape) + ")"


def buffer_info(arg):
    """Return buffer interface information (format, ndim, shape) for a
    ctypes type or instance (callproc.c buffer_info)."""
    if isinstance(arg, _CDataMeta):
        info = _info(arg)
    elif isinstance(arg, _CData):
        info = _info(type(arg))
    else:
        info = None
    if info is None:
        raise TypeError("not a ctypes type or object")
    dims = []
    t = arg if isinstance(arg, _CDataMeta) else type(arg)
    while isinstance(t, PyCArrayType):
        ti = _info_req(t)
        dims.append(ti.length)
        t = ti.proto
    return (info.format, len(dims), tuple(dims))


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


def _thunk_memoryview_at(ptr, size, readonly=False):
    # 3.14 `memoryview_at(ptr, size, readonly)`: a memoryview over raw
    # memory (`PyMemoryView_FromMemory`); the caller owns the region.
    # The declared `c_ssize_t` argtype wraps like CPython's setfunc
    # (PyLong_AsUnsignedLongLongMask): sys.maxsize + 1 arrives negative.
    bits = 8 * _PTR
    size = ((_parg_int(size) + (1 << (bits - 1))) % (1 << bits)) - (1 << (bits - 1))
    if size < 0:
        raise ValueError("memoryview_at: negative size")
    if size >= _ABSURD_SIZE:
        raise MemoryError
    return _nat.memoryview_at(_parg_addr(ptr), size, bool(_parg_int(readonly)))


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
_memoryview_at_addr = _register_thunk(_thunk_memoryview_at)


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
        if issubclass(t, (Structure, Union)):
            # By value: the bridge gets the aggregate's shape and applies
            # the platform classification (libffi's FFI_TYPE_STRUCT).
            return _agg_descr(t)
        if issubclass(t, (_Pointer, Array, CFuncPtr)):
            return "P"
    if callable(t):
        return "i"  # `restype` as a callable: raw int result, then call it
    raise TypeError("unsupported ctypes type in FFI signature: %r" % (t,))


def _agg_leaves(tp, base, out):
    """Flatten `tp` into `(offset, code)` scalar leaves for the aggregate
    descriptor (arrays and nested aggregates expanded; bitfields report
    their storage unit; pointer-ish members are `P`)."""
    info = _info_req(tp)
    if issubclass(tp, _SimpleCData):
        code = info.code
        if code == "v":
            code = "h"
        elif code in ("z", "Z", "O"):
            code = "P"
        out.append((base, code))
    elif issubclass(tp, (_Pointer, CFuncPtr)):
        out.append((base, "P"))
    elif issubclass(tp, Array):
        elem = info.proto
        esize = _info_req(elem).size
        for i in range(info.length):
            _agg_leaves(elem, base + i * esize, out)
    else:
        for fld in (info.fields or {}).values():
            _agg_leaves(fld.type, base + fld.byte_offset, out)


def _agg_descr(tp):
    """`(size, [(offset, code), ...])` for a Structure/Union type, cached
    once the layout is final."""
    info = _info_req(tp)
    descr = info.ffi_descr
    if descr is None:
        leaves = []
        _agg_leaves(tp, 0, leaves)
        descr = (info.size, tuple(leaves))
        if info.final or info.fields:
            info.ffi_descr = descr
    return descr


def _arg_to_ffi(value, index=1):
    """Marshal an argument with no declared argtype (CPython's
    ConvParam defaults). `index` is the 1-based argument position used
    in the error message."""
    if isinstance(value, _CArgObject):
        if value.tag in ("P", "z", "Z"):
            return ("P", value._address())
        return (value.tag, value._value)
    if isinstance(value, (Structure, Union)):
        # ConvParam: a CDataObject is passed by value with its own ffi type.
        info = _info_req(type(value))
        return (_agg_descr(type(value)), value._read(0, info.size))
    if isinstance(value, Array):
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
    param = getattr(value, "_as_parameter_", _MISSING)
    if param is not _MISSING:
        return _arg_to_ffi(param, index)
    raise TypeError("Don't know how to convert parameter %d" % (index,))


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
        if i < n_declared and isinstance(argtypes[i], _CDataMeta):
            code = _type_code_for_ffi(argtypes[i])
            payload = _coerce_payload(code, val)
        else:
            # Variadic tail, or an argtype that is merely an object with a
            # `from_param` method (test_parameters.test_noctypes_argtype):
            # the converted value alone decides the FFI type (ConvParam).
            try:
                code, payload = _arg_to_ffi(val, i + 1)
            except (TypeError, ValueError) as exc:
                raise ArgumentError(
                    "argument %d: %s: %s" % (i + 1, type(exc).__name__, exc))
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
    if isinstance(code, tuple):
        # By-value aggregate: the instance's raw bytes (from_param handed
        # back the instance itself, or a byref-style cparam wrapping it).
        if isinstance(value, _CArgObject):
            value = value._obj
        if not isinstance(value, (Structure, Union)):
            raise TypeError("expected a Structure/Union instance, got %s"
                            % (type(value).__name__,))
        return value._read(0, _info_req(type(value)).size)
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
        # Unwrap the cparam and normalise its payload like a bare value
        # (a `c_char` cparam carries the one-byte bytes object).
        value = value._value
        if isinstance(value, _CData):
            return value
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
    if code == "u":
        if isinstance(value, _SimpleCData):
            value = value.value
        if isinstance(value, str):
            return ord(value)
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
        # The bridge returns the register image of a wchar_t / char as an
        # int; GetResult memcpys it into the result buffer and the getfunc
        # produces the one-character str / bytes.
        if info.code == "u" and isinstance(raw, int):
            raw = chr(raw)
        elif info.code == "c" and isinstance(raw, int):
            raw = bytes([raw & 0xFF])
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
    if isinstance(restype, _CDataMeta) and issubclass(restype, (Structure, Union)):
        # GetResult: a fresh instance holding the returned bytes.
        inst = _alloc_instance(restype)
        _write_at(inst, 0, bytes(raw))
        return inst
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
        # By-value aggregate: the trampoline delivers the raw bytes it
        # reassembled from registers / stack / hidden pointer.
        inst = _alloc_instance(argtype)
        if isinstance(raw, int):
            raw = _nat.read_mem(raw, _info_req(argtype).size)
        _write_at(inst, 0, bytes(raw))
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
        if info.code == "O":
            # py_object: the trampoline mints the owned PyObject* itself.
            return result
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


class CThunkObject:
    """CPython `_ctypes.CThunkObject`: owns a callback's Python callable
    and its native closure. Freed with the thunk (test_issue_7959)."""

    __slots__ = ("_callable", "_closure", "__weakref__")
    __module__ = "_ctypes"

    def __init__(self, *args, **kwargs):
        raise TypeError("cannot create 'CThunkObject' instances")

    def __del__(self):
        closure = self._closure
        if closure:
            object.__setattr__(self, "_closure", 0)
            _nat.free_closure(closure)


def _make_closure(functype, callable_):
    # A real C-callable closure is created by the native bridge. The native
    # trampoline can only marshal primitives, so wrap the user callable so it
    # (a) rebuilds each declared argtype from the raw primitive before the
    # call and (b) reduces the return value back to a primitive afterwards.
    # Exceptions are routed to sys.unraisablehook with CPython's exact
    # message shapes (Modules/_ctypes/callbacks.c).
    #
    # The trampoline's environment is a GC-invisible strong reference, so
    # it must not hold the user callable directly: it reaches it through
    # a weakref to the thunk (which owns the callable), so a callback that
    # is only reachable through a cycle with its own CFuncPtr is
    # collectable.
    argtypes = tuple(functype._argtypes_ or ())
    restype = functype._restype_
    argcodes = [_type_code_for_ffi(t) for t in argtypes]
    rcode = _type_code_for_ffi(restype) if restype is not None else None
    thunk = object.__new__(CThunkObject)
    object.__setattr__(thunk, "_callable", callable_)
    object.__setattr__(thunk, "_closure", 0)
    owner = _weakref.ref(thunk)

    def _closure_entry(*raw):
        th = owner()
        callable_ = th._callable if th is not None else None
        try:
            if callable_ is None:
                raise RuntimeError("ctypes callback function object is dead")
            conv = [_from_closure_arg(at, val)
                    for at, val in zip(argtypes, raw)]
            if len(raw) > len(argtypes):
                conv.extend(raw[len(argtypes):])
            result = callable_(*conv)
        except BaseException as exc:
            _nat.unraisable(
                exc,
                "Exception ignored while calling ctypes callback function %r"
                % (callable_,))
            return 0
        try:
            return _to_closure_result(restype, result)
        except BaseException as exc:
            _nat.unraisable(
                exc,
                "Exception ignored while converting result of ctypes "
                "callback function %r" % (callable_,))
            return 0

    try:
        closure = _nat.create_closure(_closure_entry, rcode, argcodes)
    except NotImplementedError:
        closure = 0
    object.__setattr__(thunk, "_closure", closure)
    return thunk
