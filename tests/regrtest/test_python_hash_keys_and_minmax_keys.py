"""Keys whose `__hash__` is Python code, and `min`/`max` with getter keys.

Compiled code stores an `Enum` member or a frozen dataclass into a dict or
set (`d[k] = v`, `{k: v for ...}`, `{k for ...}`) with the hash its
`__hash__` computes, matching an entry by identity; an equal but distinct
key, a `__hash__` with side effects, and one that raises behave as before.
`min`/`max` evaluate `attrgetter`/`itemgetter` keys in place; errors from
keys and comparisons propagate as before.
"""

import dataclasses
import enum
import operator
import unittest

N = 3000


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


@dataclasses.dataclass(frozen=True)
class Pt:
    x: int
    y: int


class Counted:
    calls = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        Counted.calls += 1
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Counted) and other.v == self.v


class BadHash:
    def __hash__(self):
        raise RuntimeError("no hash")


class Obj:
    def __init__(self, x):
        self.x = x


class PythonHashKeyTests(unittest.TestCase):
    def test_enum_keys(self):
        colors = list(Color)
        for i in range(N):
            d = {c: c.value for c in colors}
            s = {c for c in colors}
            x = {}
            x[colors[i % 3]] = i
            x[colors[i % 3]] = i + 1
        self.assertEqual(d, {Color.RED: 1, Color.GREEN: 2, Color.BLUE: 3})
        self.assertEqual(s, set(colors))
        self.assertEqual(x, {colors[(N - 1) % 3]: N})

    def test_equal_distinct_keys(self):
        for i in range(N):
            d = {}
            d[Pt(1, 2)] = "a"
            d[Pt(1, 2)] = "b"
            m = {Pt(i % 2, 0): i for _ in range(3)}
            s = {Pt(1, 1) for _ in range(3)}
        self.assertEqual(d, {Pt(1, 2): "b"})
        self.assertEqual(len(d), 1)
        self.assertEqual(len(m), 1)
        self.assertEqual(s, {Pt(1, 1)})

    def test_counted_hash(self):
        Counted.calls = 0
        keys = [Counted(i) for i in range(10)]
        for _ in range(100):
            d = {k: 1 for k in keys}
        self.assertEqual(len(d), 10)
        self.assertEqual(Counted.calls, 1000)
        Counted.calls = 0
        x = {}
        for k in keys:
            x[k] = 0
        self.assertEqual(Counted.calls, 10)

    def test_raising_hash(self):
        b = BadHash()
        for _ in range(200):
            with self.assertRaises(RuntimeError):
                {b: 1}
            with self.assertRaises(RuntimeError):
                {b for _ in range(1)}
            d = {}
            with self.assertRaises(RuntimeError):
                d[b] = 1
            self.assertEqual(d, {})


class MinMaxKeyTests(unittest.TestCase):
    def test_getter_keys(self):
        objs = [Obj(v) for v in (3, 9, 1, 9, 4)]
        pairs = [(i, -i) for i in range(10)]
        ag = operator.attrgetter("x")
        ig = operator.itemgetter(1)
        for _ in range(N):
            r = (max(objs, key=ag), min(objs, key=ag), max(pairs, key=ig), min(pairs, key=ig))
        self.assertIs(r[0], objs[1])
        self.assertIs(r[1], objs[2])
        self.assertEqual(r[2:], ((0, 0), (9, -9)))

    def test_key_errors(self):
        ag = operator.attrgetter("missing")
        for _ in range(200):
            with self.assertRaises(AttributeError):
                max([Obj(1), Obj(2)], key=ag)
            with self.assertRaises(IndexError):
                min([(1,), ()], key=operator.itemgetter(0))
            with self.assertRaises(TypeError):
                max([Obj(1), Obj("a")], key=operator.attrgetter("x"))
        self.assertEqual(max([], key=ag, default="d"), "d")
        self.assertEqual(max([Obj(5)], key=operator.attrgetter("x")).x, 5)
        with self.assertRaises(AttributeError):
            max([Obj(5)], key=ag)

    def test_mixed_and_pure_keys(self):
        objs = [Obj(v) for v in range(20)]
        for _ in range(N):
            r = (max(objs, key=lambda o: -o.x), min(objs, key=lambda o: o.x % 7))
        self.assertEqual((r[0].x, r[1].x), (0, 0))


if __name__ == "__main__":
    unittest.main()
