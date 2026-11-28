"""`setattr(obj, name, value)` replacing an attribute already there.

The builtin replaces an existing instance attribute at its position;
new attributes, re-added ones, `__dict__` order, finalizers of replaced
values, and descriptors behave as before.
"""

import unittest

N = 3000


class O:
    def __init__(self):
        self.x = 1
        self.y = 2


class Dying:
    deaths = 0

    def __del__(self):
        Dying.deaths += 1


class P:
    @property
    def v(self):
        return self._v

    @v.setter
    def v(self, value):
        self._v = value * 2


class SetattrReplaceTests(unittest.TestCase):
    def test_replace_and_add(self):
        o = O()
        for i in range(N):
            setattr(o, "x", i)
        self.assertEqual((o.x, o.y), (N - 1, 2))
        setattr(o, "z", 3)
        self.assertEqual(list(o.__dict__), ["x", "y", "z"])
        del o.x
        setattr(o, "x", 9)
        self.assertEqual(list(o.__dict__), ["y", "z", "x"])
        name = "".join(["y"])
        setattr(o, name, 5)
        self.assertEqual(o.y, 5)

    def test_replaced_value_finalizes(self):
        o = O()
        Dying.deaths = 0
        for i in range(N // 10):
            setattr(o, "x", Dying())
            self.assertEqual(Dying.deaths, i)
        o.x = None
        self.assertEqual(Dying.deaths, N // 10)

    def test_descriptor(self):
        p = P()
        for i in range(N // 10):
            setattr(p, "v", i)
        self.assertEqual(p.v, 2 * (N // 10 - 1))


if __name__ == "__main__":
    unittest.main()
