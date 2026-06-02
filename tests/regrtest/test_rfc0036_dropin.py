"""RFC 0036 regression guard.

Locks in the CPython-compatibility fixes landed in the Lib/test conformance
sweep so they can't silently regress. Every section maps to a specific bug
found while running CPython 3.13's own `Lib/test/` files under WeavePy.
Plain `assert`s only — the file exits 0 iff every behaviour matches CPython.
"""

# ---------------------------------------------------------------------------
# Trailing-dot float literals — `1.`, `2.+3.`, `[1.]`, `1.e3`.
# (lexer: a `.` right after the integer part always belongs to the float.)
# ---------------------------------------------------------------------------
assert 1. == 1.0
assert 2. + 3. == 5.0
assert [1., 2.] == [1.0, 2.0]
assert 1.e3 == 1000.0
assert (1.).is_integer()

# ---------------------------------------------------------------------------
# PEP 448 — iterable unpacking in list / set / tuple displays.
# ---------------------------------------------------------------------------
assert [1, *[2, 3], 4] == [1, 2, 3, 4]
assert [*range(3)] == [0, 1, 2]
assert (1, *[2, 3]) == (1, 2, 3)
assert (*[1], *[2, 3]) == (1, 2, 3)
assert {1, *[2, 3], *{3, 4}} == {1, 2, 3, 4}
assert [*"ab", *"cd"] == ["a", "b", "c", "d"]
# A lone splat in parens is still a syntax error (stays a generator-free
# expression), but unpacking with a trailing comma is a 1-tuple build:
assert (*[1, 2],) == (1, 2)

# ---------------------------------------------------------------------------
# `\N{NAME}` named Unicode escapes (full UCD name table).
# ---------------------------------------------------------------------------
assert "\N{BULLET}" == "\u2022"
assert "\N{NO-BREAK SPACE}" == "\xa0"
assert "\N{GREEK SMALL LETTER ALPHA}" == "\u03b1"
assert "\N{GRINNING FACE}" == "\U0001f600"
assert len("\N{NARROW NO-BREAK SPACE}") == 1

# ---------------------------------------------------------------------------
# `__debug__` builtin constant (True without `-O`).
# ---------------------------------------------------------------------------
assert __debug__ is True

# ---------------------------------------------------------------------------
# `sys.float_info` / `int_info` / `hash_info` answer attribute access
# (struct-sequence shape), and `sys.float_repr_style` exists.
# ---------------------------------------------------------------------------
import sys

assert sys.float_info.max > 1e308
assert sys.float_info.min > 0.0
assert sys.float_info.epsilon > 0.0
assert sys.float_info.mant_dig == 53
assert sys.float_info.radix == 2
assert sys.int_info.bits_per_digit > 0
assert sys.hash_info.hash_bits == 64
assert sys.float_repr_style == "short"

# ---------------------------------------------------------------------------
# Class `__doc__` — leading string literal becomes the docstring; `None`
# otherwise (never an AttributeError).
# ---------------------------------------------------------------------------
class _Documented:
    "the docstring"

class _Undocumented:
    pass

assert _Documented.__doc__ == "the docstring"
assert _Undocumented.__doc__ is None

# ---------------------------------------------------------------------------
# `unicodedata.name` / `.lookup` use the full UCD name table.
# ---------------------------------------------------------------------------
import unicodedata

assert unicodedata.name("a") == "LATIN SMALL LETTER A"
assert unicodedata.name("\u2022") == "BULLET"
assert unicodedata.lookup("BULLET") == "\u2022"
assert unicodedata.lookup("NO-BREAK SPACE") == "\xa0"

# ---------------------------------------------------------------------------
# `zip()` with no arguments is an empty iterator (must not hang).
# ---------------------------------------------------------------------------
assert list(zip()) == []
assert list(zip([1, 2], [3, 4])) == [(1, 3), (2, 4)]

# ---------------------------------------------------------------------------
# A native iterator is its own iterable: `iter(it) is it`, and it can be
# drained by plain builtins (`dict.fromkeys`, `set`, `list`).
# ---------------------------------------------------------------------------
_it = iter([1, 2, 3])
assert iter(_it) is _it
assert dict.fromkeys(iter([1, 2, 3])) == {1: None, 2: None, 3: None}
assert set(map(str, [1, 2, 2])) == {"1", "2"}
assert list(map(lambda x: x * 2, iter([1, 2, 3]))) == [2, 4, 6]

# ---------------------------------------------------------------------------
# PEP 487 — `__init_subclass__` / `__class_getitem__` defined as plain
# `def`s become implicit class methods.
# ---------------------------------------------------------------------------
class _Base:
    registered = []

    def __init_subclass__(cls, **kwargs):
        _Base.registered.append(cls.__name__)


class _Sub(_Base):
    pass


assert "_Sub" in _Base.registered


class _Generic:
    def __class_getitem__(cls, item):
        return ("_Generic", item)


assert _Generic[int] == ("_Generic", int)

# ---------------------------------------------------------------------------
# `string.Template` imports and substitutes (its class body calls
# `__init_subclass__` at import time — the motivating case).
# ---------------------------------------------------------------------------
from string import Template

assert Template("$who likes $what").substitute(who="tim", what="kung pao") == \
    "tim likes kung pao"

print("ok")
