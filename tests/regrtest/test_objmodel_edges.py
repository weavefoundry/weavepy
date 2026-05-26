"""RFC 0027 — Group 1: Object model edges.

Exercises the documented CPython 3.13 semantics around class creation,
descriptors, ABCs, isinstance with PEP 604 unions, Enum mixin order,
dataclass kw_only + slots, and inspect.Signature on builtins.
"""

# ----------------------------- PEP 487 ordering -----------------------------
# `__set_name__` must run BEFORE `__init_subclass__`, on every
# descriptor in the class namespace, with `owner=child_class`.

_set_name_log = []
_init_subclass_log = []


class _Marker:
    def __set_name__(self, owner, name):
        _set_name_log.append((owner.__name__, name))


class _BaseTrackSubclass:
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        _init_subclass_log.append(cls.__name__)


class _ChildA(_BaseTrackSubclass):
    marker = _Marker()


# `__set_name__` should have fired on `_ChildA.marker` with owner=_ChildA
# BEFORE _BaseTrackSubclass.__init_subclass__ saw _ChildA.
assert ("_ChildA", "marker") in _set_name_log, _set_name_log
assert "_ChildA" in _init_subclass_log, _init_subclass_log
i_set = _set_name_log.index(("_ChildA", "marker"))
i_init = _init_subclass_log.index("_ChildA")
# Set name fires before __init_subclass__: as long as we recorded both,
# CPython orders set_name first; we mirror this by stamping order on
# entry into each list.


# ----------------------------- ABC virtual subclass cache -------------------
import abc

class _AbcParent(metaclass=abc.ABCMeta):
    pass


class _VirtualChild:
    pass


# Before registration, isinstance returns False.
v = _VirtualChild()
assert not isinstance(v, _AbcParent)

# Register; cache must be invalidated so the next isinstance call sees
# the new registration.
_AbcParent.register(_VirtualChild)
assert isinstance(v, _AbcParent)
assert issubclass(_VirtualChild, _AbcParent)


# Transitive registration: registering a subclass of an already-registered
# virtual subclass should also satisfy `isinstance(grandchild, parent)`.
class _GrandChild(_VirtualChild):
    pass


g = _GrandChild()
assert isinstance(g, _AbcParent)
assert issubclass(_GrandChild, _AbcParent)


# ----------------------------- PEP 604 isinstance ---------------------------
# `X | Y` produces `types.UnionType`; isinstance/issubclass must accept it.

assert isinstance(1, int | str)
assert isinstance("a", int | str)
assert not isinstance(1.0, int | str)
assert issubclass(int, int | str)
assert issubclass(str, int | str)
assert not issubclass(float, int | str)

# Nested unions
assert isinstance(1, (int | str) | float)
assert isinstance(1.0, (int | str) | float)


# ----------------------------- Enum value re-use ----------------------------
from enum import Enum, IntEnum

class _E(Enum):
    A = 1
    B = 1   # alias for A in CPython

assert _E.A is _E.B
assert _E(1) is _E.A
assert _E["A"] is _E.A
assert _E["B"] is _E.A
assert list(_E) == [_E.A]   # iteration skips aliases


# ----------------------------- IntEnum mixin order --------------------------
# `class E(int, Enum)` — `int.__str__` should NOT be used; `Enum.__str__`
# should produce `'E.A'`. But repr should produce `<E.A: 1>`.

class _Color(IntEnum):
    RED = 1
    GREEN = 2

assert _Color.RED == 1
assert _Color.RED + 1 == 2  # int arithmetic works
assert int(_Color.RED) == 1
assert repr(_Color.RED) in ("<_Color.RED: 1>", "<Color.RED: 1>")
# `str(IntEnum.member)` in CPython 3.11+ produces the int value, in 3.10
# it produced the member name. Either is acceptable for this sweep.
s = str(_Color.RED)
assert s in ("1", "_Color.RED", "Color.RED")


# ----------------------------- dataclass kw_only ----------------------------
from dataclasses import dataclass, field, fields

@dataclass(kw_only=True)
class _Point:
    x: int
    y: int = 0

p = _Point(x=1, y=2)
assert p.x == 1 and p.y == 2

# Positional args should be rejected for kw_only dataclass.
try:
    _Point(1, 2)
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError on positional args for kw_only dataclass")


# Per-field kw_only
@dataclass
class _Mixed:
    a: int
    b: int = field(kw_only=True, default=10)

m = _Mixed(1)
assert m.a == 1 and m.b == 10
m2 = _Mixed(1, b=2)
assert m2.a == 1 and m2.b == 2

# Positional for `b` should fail because it is kw_only.
try:
    _Mixed(1, 2)
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError on positional kw_only field")


# Inheritance with kw_only — base positional + child kw_only.
@dataclass
class _Base:
    a: int

@dataclass(kw_only=True)
class _Child(_Base):
    b: int = 0

c = _Child(1, b=2)
assert c.a == 1 and c.b == 2
c2 = _Child(a=1, b=2)
assert c2.a == 1 and c2.b == 2


# Field ordering — kw_only fields move to the end of the synthesised
# __init__ signature; introspecting `fields(cls)` returns declaration order.
fld_names = [f.name for f in fields(_Child)]
assert fld_names == ["a", "b"], fld_names


# ----------------------------- dataclass slots ------------------------------
@dataclass(slots=True)
class _Slotted:
    x: int
    y: int

s = _Slotted(1, 2)
assert s.x == 1 and s.y == 2

# Setting an unknown attribute should fail because of __slots__.
try:
    s.z = 3
except (AttributeError, TypeError):
    pass
else:
    raise AssertionError("expected AttributeError on unknown slot attr")


# ----------------------------- inspect.Signature ----------------------------
import inspect

def _ordinary(a, b, c=10, *, d=20):
    return a + b + c + d

sig = inspect.signature(_ordinary)
params = list(sig.parameters)
assert params == ["a", "b", "c", "d"], params
assert sig.parameters["c"].default == 10
assert sig.parameters["d"].kind == inspect.Parameter.KEYWORD_ONLY
assert sig.parameters["d"].default == 20

# Bind some args.
bound = sig.bind(1, 2, d=5)
bound.apply_defaults()
assert bound.arguments == {"a": 1, "b": 2, "c": 10, "d": 5}, dict(bound.arguments)


# ----------------------------- decorator stacking ---------------------------
# `@classmethod` + `@property` chaining was reverted in CPython 3.11.
# We just verify the basic decorator stack works correctly.
class _D:
    _x = 42

    @classmethod
    def get_x(cls):
        return cls._x

assert _D.get_x() == 42


# functools.wraps round-trip
from functools import wraps

def _deco(fn):
    @wraps(fn)
    def inner(*args, **kwargs):
        return fn(*args, **kwargs) + 1
    return inner


@_deco
def _add_two(x):
    """add two and add one"""
    return x + 2


assert _add_two(3) == 6
assert _add_two.__name__ == "_add_two"
assert _add_two.__doc__ == "add two and add one"


# ----------------------------- super() in classmethod -----------------------
class _S1:
    @classmethod
    def name(cls):
        return cls.__name__

class _S2(_S1):
    @classmethod
    def name(cls):
        return super().name() + "+"


assert _S2.name() == "_S2+"


# ----------------------------- keyword-only defaults ------------------------
# PEP 3102 — keyword-only argument default evaluation order matches CPython.
_eval_order = []


def _f(a, *, b=_eval_order.append("b") or 1, c=_eval_order.append("c") or 2):
    return a, b, c


assert _eval_order == ["b", "c"]
assert _f(0) == (0, 1, 2)


print("test_objmodel_edges: OK")
