"""Creating classes at runtime: plain, dataclasses, enums, namedtuples, and
__init_subclass__ hooks; plus instantiating them."""

import enum
from collections import namedtuple
from dataclasses import dataclass

WORK = 120


class Plugin:
    registry = {}

    def __init_subclass__(cls, key=None, **kw):
        super().__init_subclass__(**kw)
        Plugin.registry[key or cls.__name__] = cls


def bench(n):
    total = 0
    for i in range(n):
        C = type("C%d" % i, (Plugin,), {"x": i, "get": lambda self: self.x})
        total += C().get()

        @dataclass(frozen=(i % 2 == 0))
        class D:
            a: int
            b: str = "b"
            c: float = 0.5

        total += D(i).a + len(repr(D(1, "z")))
        E = enum.Enum("E%d" % (i % 10), "RED GREEN BLUE")
        total += E.GREEN.value + len(E)
        NT = namedtuple("NT", ["p", "q"])
        total += NT(1, 2).q
    return total + len(Plugin.registry) % 7
