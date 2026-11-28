"""Builtin lookups after annotationlib evaluated a class's annotations
with a dict subclass's __missing__ as globals (a typing.NamedTuple, a
dataclass with a forward reference): compiled loops that only read
builtins still see them, a live exec namespace whose __missing__ serves
names (builtins' too, as in CPython) keeps serving them, and once it is gone the builtins answer again,
each matching CPython."""

import dataclasses
import typing
import unittest


class TD(typing.NamedTuple):
    a: int
    b: "Later" = None


@dataclasses.dataclass
class DC:
    x: "Later"


class Later:
    pass


class Fallback(dict):
    def __missing__(self, key):
        return "missing:" + key


def loop(n):
    t = 0
    xs = [1, 2]
    for i in range(n):
        t += len(xs) + abs(-1) + hash(1)
    return t


class GateTests(unittest.TestCase):
    def test_builtins_after_annotations(self):
        self.assertEqual(TD(1), (1, None))
        self.assertEqual(loop(5000), 5000 * 4)

    def test_live_missing_namespace(self):
        ns = Fallback(__builtins__=__builtins__)
        exec("def f(n):\n    out = None\n    while n:\n        out = (len, undefined_name)\n        n -= 1\n    return out", ns)
        self.assertEqual(ns["f"](3000), ("missing:len", "missing:undefined_name"))
        self.assertEqual(loop(3000), 3000 * 4)
        self.assertEqual(ns["f"](10), ("missing:len", "missing:undefined_name"))
        del ns
        self.assertEqual(loop(3000), 3000 * 4)


if __name__ == "__main__":
    unittest.main()
