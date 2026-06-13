"""RFC 0037 regression guard — CPython Lib/test conformance sweep, wave 2.

Locks in the object-model, exception, numeric and stdlib fixes landed while
running CPython 3.13's own `Lib/test/` files under WeavePy, so they can't
silently regress. Every section maps to a workstream (WS1–WS9) in the RFC and
to a concrete bug found in the measured sweep. Plain `assert`s only — the file
exits 0 iff every behaviour matches CPython.
"""

import sys
import types

# ---------------------------------------------------------------------------
# WS1 — recursion guard raises RecursionError (not a native stack overflow).
# ---------------------------------------------------------------------------
def _blow():
    return _blow()


try:
    _blow()
except RecursionError:
    pass
else:
    raise AssertionError("expected RecursionError")

# ---------------------------------------------------------------------------
# Object model — unbound instance methods reached via the *type*.
# `str.upper(x)` / `float.hex(x)` / `list.append(l, v)` all take `self`
# explicitly, exactly like CPython's method-descriptors.
# ---------------------------------------------------------------------------
assert str.upper("hi") == "HI"
assert str.capitalize("hi") == "Hi"
assert str.split("a b c") == ["a", "b", "c"]
assert float.hex(1.5) == "0x1.8p+0"
assert int.bit_length(255) == 8
assert bytes.hex(b"\x01\x02") == "0102"
assert dict.get({"a": 1}, "a") == 1
_l = [1]
list.append(_l, 2)
assert _l == [1, 2]
# The same descriptor is shared by bound and unbound forms.
assert "hi".upper() == str.upper("hi")

# ---------------------------------------------------------------------------
# WS3 — numeric protocol surface.
# ---------------------------------------------------------------------------
assert (1.5).hex() == "0x1.8p+0"
assert float.fromhex("0x1.8p+0") == 1.5
assert (3.0).__trunc__() == 3
assert (3.7).__floor__() == 3
assert (3.2).__ceil__() == 4
assert (-3.2).__floor__() == -4
# complex.__complex__ returns the value unchanged.
assert complex(3, 4).__complex__() == (3 + 4j)
assert (3 + 4j).conjugate() == (3 - 4j)

# ---------------------------------------------------------------------------
# WS5 — exception attribute slots + PEP 678 notes + with_traceback.
# Every BaseException carries __context__/__cause__/__suppress_context__/
# __traceback__ from birth, so context-chaining helpers never AttributeError.
# ---------------------------------------------------------------------------
_e = ValueError("x")
assert _e.__context__ is None
assert _e.__cause__ is None
assert _e.__suppress_context__ is False
assert _e.__traceback__ is None
# with_traceback returns self and sets __traceback__.
assert _e.with_traceback(None) is _e
# add_note appends to __notes__ (PEP 678).
_e.add_note("note-1")
assert _e.__notes__ == ["note-1"]

# sys.exception() returns the exception currently being handled (PEP 3134 era).
try:
    raise KeyError("k")
except KeyError as caught:
    assert sys.exception() is caught

# Implicit exception context chaining sets __context__.
try:
    try:
        raise ValueError("inner")
    except ValueError:
        raise TypeError("outer")
except TypeError as outer:
    assert isinstance(outer.__context__, ValueError)

# ---------------------------------------------------------------------------
# WS6 — class machinery: types.MethodType is a callable constructor that
# binds self, and __module__ resolves on both builtin and user types.
# ---------------------------------------------------------------------------
def _f(self, x):
    return (self, x)


_bound = types.MethodType(_f, "recv")
assert callable(_bound)
assert type(_bound).__name__ == "method"
assert _bound(42) == ("recv", 42)

assert object.__module__ == "builtins"
assert int.__module__ == "builtins"


class _UserClass:
    pass


assert _UserClass.__module__ == "__main__"

# ---------------------------------------------------------------------------
# Object model — dict(**kwargs) / dict(mapping, **kwargs) constructors.
# ---------------------------------------------------------------------------
assert dict(a=1, b=2) == {"a": 1, "b": 2}
assert dict({"x": 1}, y=2) == {"x": 1, "y": 2}
assert dict([("p", 1)], q=2) == {"p": 1, "q": 2}

# ---------------------------------------------------------------------------
# Object model — sys.flags is a struct-sequence (attribute access), and the
# int<->str conversion cap round-trips.
# ---------------------------------------------------------------------------
assert isinstance(sys.flags.optimize, int)
assert isinstance(sys.flags.bytes_warning, int)
assert sys.get_int_max_str_digits() >= 640
_orig = sys.get_int_max_str_digits()
sys.set_int_max_str_digits(1000)
assert sys.get_int_max_str_digits() == 1000
sys.set_int_max_str_digits(_orig)

# ---------------------------------------------------------------------------
# Compiler — a function whose control flow ends inside nested conditionals
# still falls through to an implicit `return None` (no "pc out of bounds").
# ---------------------------------------------------------------------------
def _implicit_return(a, b):
    if a:
        if b:
            pass
        else:
            return "x"


assert _implicit_return(True, True) is None
assert _implicit_return(False, False) is None
assert _implicit_return(True, False) == "x"

# ---------------------------------------------------------------------------
# WS7 — verbatim CPython contextlib gains asynccontextmanager / AsyncExitStack.
# ---------------------------------------------------------------------------
import contextlib

assert hasattr(contextlib, "asynccontextmanager")
assert hasattr(contextlib, "AsyncExitStack")
assert hasattr(contextlib, "aclosing")

# ---------------------------------------------------------------------------
# WS8 — collections package: abc + UserDict/UserList/UserString.
# ---------------------------------------------------------------------------
import collections
import collections.abc as cabc

assert isinstance({}, cabc.Mapping)
assert isinstance([], cabc.Sequence)
assert issubclass(dict, cabc.MutableMapping)

_ud = collections.UserDict({"a": 1})
_ud["b"] = 2
assert _ud["a"] == 1 and _ud["b"] == 2
assert collections.UserList([1, 2]) + [3] == [1, 2, 3]
assert collections.UserString("ab").upper() == "AB"

print("ok")
