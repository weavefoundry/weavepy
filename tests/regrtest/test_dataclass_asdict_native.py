"""dataclasses.asdict over trees of plain values, nested dataclasses and
exact containers, from compiled loops: fresh copies of every container,
fields in order, ClassVar and InitVar left out, and the Python path still
taking what the native copy declines (a namedtuple, a set, an enum, a
custom dict_factory, a slots dataclass, a deleted field, a property
field, a cycle, a profiler watching), each matching CPython."""

import collections
import dataclasses
import enum
import sys
import unittest
from typing import ClassVar


@dataclasses.dataclass
class Inner:
    a: int
    b: str = "x"


@dataclasses.dataclass
class Outer:
    name: str
    inner: Inner
    items: list = dataclasses.field(default_factory=list)
    meta: dict = dataclasses.field(default_factory=dict)
    pair: tuple = (1, 2)
    count: ClassVar[int] = 0
    flag: bool = False
    ratio: float = 0.5
    blob: bytes = b""
    nothing: object = None


NT = collections.namedtuple("NT", "p q")


class Color(enum.Enum):
    RED = 1


@dataclasses.dataclass
class Mixed:
    nt: NT
    tags: set
    color: Color


@dataclasses.dataclass(slots=True)
class Slotted:
    a: int
    b: list


@dataclasses.dataclass
class WithInit:
    a: int
    scale: dataclasses.InitVar[int] = 1

    def __post_init__(self, scale):
        self.a *= scale


def copies(n, obj, **kw):
    out = None
    for i in range(n):
        out = dataclasses.asdict(obj, **kw)
    return out


class AsdictTests(unittest.TestCase):
    def test_plain_tree(self):
        inner = Inner(1)
        o = Outer("o", inner, [Inner(2, "y"), [3, (4, Inner(5))]], {"k": Inner(6), 7: [8]})
        want = {
            "name": "o",
            "inner": {"a": 1, "b": "x"},
            "items": [{"a": 2, "b": "y"}, [3, (4, {"a": 5, "b": "x"})]],
            "meta": {"k": {"a": 6, "b": "x"}, 7: [8]},
            "pair": (1, 2),
            "flag": False,
            "ratio": 0.5,
            "blob": b"",
            "nothing": None,
        }
        for n in (1, 300):
            got = copies(n, o)
            self.assertEqual(got, want)
            self.assertEqual(list(got), list(want))
            self.assertIsNot(got["items"], o.items)
            self.assertIsNot(got["meta"][7], o.meta[7])
            self.assertIs(type(got["pair"]), tuple)

    def test_declined_shapes(self):
        m = Mixed(NT(1, Inner(2)), {1, 2}, Color.RED)
        for n in (1, 300):
            got = copies(n, m)
            self.assertEqual(got, {"nt": NT(1, {"a": 2, "b": "x"}), "tags": {1, 2}, "color": Color.RED})
            self.assertIs(type(got["nt"]), NT)
            self.assertIsNot(got["tags"], m.tags)
            got = copies(n, Inner(1), dict_factory=collections.OrderedDict)
            self.assertIs(type(got), collections.OrderedDict)
            s = Slotted(1, [2])
            self.assertEqual(copies(n, s), {"a": 1, "b": [2]})
            self.assertEqual(copies(n, WithInit(2, 3)), {"a": 6})

    def test_deleted_field_and_cycle(self):
        i = Inner(1)
        del i.a
        with self.assertRaises(AttributeError):
            copies(300, i)
        o = Outer("c", Inner(1))
        o.items.append(o.items)
        with self.assertRaises(RecursionError):
            copies(1, o)

    def test_property_field(self):
        @dataclasses.dataclass
        class P:
            x: int

        P.x = property(lambda self: 42, lambda self, v: None)
        p = P(1)
        self.assertEqual(copies(300, p), {"x": 42})

    def test_errors(self):
        with self.assertRaises(TypeError):
            dataclasses.asdict(Inner)
        with self.assertRaises(TypeError):
            dataclasses.asdict(3)

    def test_profiler_sees_python_copy(self):
        copies(300, Inner(1))
        seen = []

        def prof(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)

        sys.setprofile(prof)
        try:
            dataclasses.asdict(Inner(1))
        finally:
            sys.setprofile(None)
        self.assertIn("_asdict_inner", seen)


if __name__ == "__main__":
    unittest.main()
