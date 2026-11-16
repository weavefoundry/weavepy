"""Attribute reads of namespace-backed objects in compiled code.

`typing.get_type_hints` reads `t.__args__` of a union, which WeavePy keeps
in a namespace, as `types.SimpleNamespace` keeps its fields. Compiled code
reads an entry the namespace holds in place; a missing one still raises
`AttributeError`, and the union's computed attributes still answer. Items
are heap objects. Run it past the compile threshold.
"""

import types
import typing


class Item:
    pass


def args_of(t):
    return t.__args__


def field(ns):
    return ns.a


def missing(ns):
    try:
        return ns.zz
    except AttributeError:
        return "missing"


item = Item()


def f(a: int, b: typing.Optional[str] = None) -> typing.Optional[int]:
    return None


for _ in range(3000):
    assert args_of(typing.Optional[int]) == (int, type(None))
    assert args_of(int | str) == (int, str)
    ns = types.SimpleNamespace(a=item)
    assert field(ns) is item and missing(ns) == "missing"
    ns.a = 5
    assert field(ns) == 5
    assert typing.get_origin(int | None) is typing.Union
    hints = typing.get_type_hints(f)
    assert hints["b"] == typing.Optional[str] and hints["return"] == int | None
