"""RFC 0076 WS1–WS4 — engine fixes surfaced by the selftest burns.

One bundled canary per fix:

1. Zero-arg `super()` captures `__class__` through *normal* lexical
   scoping, not just a class body: a nested function compiled inside
   `def wrapper(_cls): __class__ = _cls; def m(self): … super() …`
   must close over the wrapper's local (promoted to a cell *before*
   emission, so the `__class__ = _cls` store lands in the cell).
   attrs' generated slots-`__getattr__` is exactly this shape — its
   cached-property tests raised "super(): __class__ cell not found",
   then (post-gate-fix) "cannot access free variable '__class__'".

2. The same shape routed through `exec` of *generated source* (attrs
   compiles the wrapper with `compile()`/`eval` at class-build time),
   including an `AttributeError`-raising property re-raised verbatim
   through the generated `__getattr__`.

3. Marshaling a large scalar into a C call must not retain its mirror
   forever: the scalar-pin cache and the `PyUnicode_AsUTF8`/
   `PyBytes_AsString` C-string cache are byte-accounted and sweep dead
   entries (Pillow's default-font leak test grew ~60 KB per
   `draw.text` call and blew its 1 MB ceiling). In-tree smoke: repeated
   `_testbuffer.ndarray` construction over fresh 128 KB bytes buffers
   stays RSS-bounded. The strong net is the Pillow selftest lane.
"""

import sys

# ------------- 1. super() through an enclosing-function local -------------


def _wrapper(_cls):
    __class__ = _cls  # noqa: F841 — the implicit super() cell

    def method(self):
        return super().__repr__()

    return method


class _Plain:
    pass


_Plain.m = _wrapper(_Plain)
_r = _Plain().m()
assert _r.startswith("<"), _r

# ------------- 2. the attrs generated-__getattr__ shape -------------------

_SRC = """
def wrapper(_cls, _cached_properties, _original_getattr):
    __class__ = _cls

    def __getattr__(self, item):
        func = _cached_properties.get(item)
        if func is not None:
            result = func(self)
            object.__setattr__(self, item, result)
            return result
        if _original_getattr is not None:
            return _original_getattr(self, item)
        try:
            return super().__getattribute__(item)
        except AttributeError:
            raise AttributeError(
                f"'{type(self).__name__}' object has no attribute '{item}'"
            ) from None

    return __getattr__
"""


class _Slotted:
    __slots__ = ("__dict__",)

    def __init__(self):
        self.calls = 0


def _prop(self):
    self.calls += 1
    return 42


_globs = {}
exec(compile(_SRC, "<generated>", "exec"), _globs)
_Slotted.__getattr__ = _globs["wrapper"](_Slotted, {"answer": _prop}, None)

_obj = _Slotted()
assert _obj.answer == 42
assert _obj.answer == 42  # cached: the property ran once
assert _obj.calls == 1, _obj.calls
try:
    _obj.missing
except AttributeError as e:
    assert "no attribute 'missing'" in str(e), e
else:
    raise AssertionError("missing attribute lookup did not raise")

# ------------- 3. bounded C-marshal retention for large scalars -----------

try:
    import _testbuffer
except ImportError:
    _testbuffer = None

if _testbuffer is not None and hasattr(sys, "getrefcount"):
    from resource import RUSAGE_SELF, getrusage

    def _mem_kb():
        m = getrusage(RUSAGE_SELF).ru_maxrss
        return m / 1024 if sys.platform == "darwin" else m

    # Warm allocator pools / import machinery before the baseline.
    for _ in range(8):
        _testbuffer.ndarray(bytes(131072))

    _start = _mem_kb()
    for i in range(200):
        # A fresh 128 KB buffer each round (re-exported through the
        # buffer protocol): 200 pinned-forever mirrors would retain
        # ~25 MB; byte-accounted sweeps keep it bounded.
        buf = bytes([i & 0xFF]) * 131072
        nd = _testbuffer.ndarray(buf)
        del nd, buf
    _grew = _mem_kb() - _start
    assert _grew < 16384, f"C-marshal retention grew {_grew:.0f} KB"

# ------------- 4. unicodedata accepts str subclasses -----------------------
#
# CPython's argument clinic checks `PyUnicode_Check`, which is
# subtype-inclusive: `unicodedata.normalize`/`is_normalized` must accept a
# `str` *subclass* (numpy's `str_` scalar — test_multiarray's
# `TestUnicodeEncoding.test_round_trip` raised "normalize() unistr must be
# str").

import unicodedata


class _S(str):
    pass


assert unicodedata.normalize("NFC", _S("cafe\u0301")) == "caf\u00e9"
assert unicodedata.is_normalized("NFC", _S("caf\u00e9"))

# ------------- 5. inherited seq wrapper defers to the reflected dunder -----
#
# CPython's `binary_op1` consults only the *number* slots; `sq_concat`/
# `sq_repeat` are a `PyNumber_Add`/`Multiply` last resort. So when a str
# subclass inherits `str.__add__` (a wrapper that *raises* "can only
# concatenate str …" rather than returning NotImplemented) and the partner
# is another class with `__radd__`, the reflected method must win —
# `np.str_ + ndarray` concatenates through ndarray's `__radd__` (numpy
# test_strings' partition round-trip raised through the wrapper instead).


class _StrSub(str):
    pass


class _RAdd:
    def __radd__(self, other):
        return "radd:" + other

    def __rmul__(self, other):
        return "rmul"


assert _StrSub("x") + _RAdd() == "radd:x"
assert _StrSub("x") * _RAdd() == "rmul"
# Same-class operands keep the wrapper (never reordered):
assert _StrSub("a") + _StrSub("b") == "ab"
# …and the wrapper is still the faithful last resort when the partner
# offers no reflected method:
try:
    _StrSub("a") + 1
except TypeError as e:
    assert "can only concatenate str" in str(e), e
else:
    raise AssertionError("str-subclass + int did not raise")

# ------------- 6. _PyUnicode_IsTitlecase knows category Lt ------------------
#
# The C export was hardwired false, so numpy's `Py_UNICODE_ISTITLE` loop
# denied every titlecase leader (U+01C5 'ǅ', U+1FFC 'ῼ' —
# test_strings' `test_istitle_unicode`). Exercise it through ctypes.

import ctypes

try:
    _fn = ctypes.pythonapi._PyUnicode_IsTitlecase
except AttributeError:
    _fn = None

if _fn is not None:
    _fn.restype = ctypes.c_int
    _fn.argtypes = [ctypes.c_uint32]
    assert _fn(0x1FFC) == 1, "U+1FFC (Lt) must be titlecase"
    assert _fn(0x01C5) == 1, "U+01C5 (Lt) must be titlecase"
    assert _fn(ord("A")) == 0, "'A' (Lu) is not titlecase"
    assert _fn(ord("a")) == 0, "'a' (Ll) is not titlecase"

print("rfc0076-burn-regressions: ok")
