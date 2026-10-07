"""Module attribute calls and global receivers in hot loops.

`module.func(...)` loads the attribute off the site's cached index; a
`math` attribute that isn't a native intrinsic leaves the loop to the
generic path; a global `__slots__` instance's member reads in place; and
a loop reading the same value each iteration shares one pinned result.
Rebinding a module attribute, or the module itself, must be seen at once.
"""

import math
import os.path
import types
import unittest


class P:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


GP = P(3, 4)
GM = types.ModuleType("gm")
GM.f = lambda v: v + 1


def floors(n):
    s = 0
    for i in range(n):
        s += math.floor(i / 2) + math.ceil(0.5)
    return s


def joins(n):
    out = 0
    for i in range(n):
        out += len(os.path.join("a", str(i)))
    return out


def slot_reads(n):
    s = 0
    for _ in range(n):
        s += GP.x + GP.y
    return s


def module_calls(n):
    s = 0
    for i in range(n):
        s += GM.f(i)
    return s


def mixed_math(n):
    s = 0.0
    for i in range(n):
        s += math.sqrt(i) + math.pi
    return s


class ModuleMethodLoadTest(unittest.TestCase):
    def test_math_attributes(self):
        for n in (10, 5000):
            self.assertEqual(floors(n), sum(i // 2 + 1 for i in range(n)))
        self.assertAlmostEqual(
            mixed_math(3000), sum(math.sqrt(i) + math.pi for i in range(3000))
        )

    def test_package_module_attribute(self):
        self.assertEqual(joins(3000), sum(len("a/" + str(i)) for i in range(3000)))

    def test_rebinding_is_seen(self):
        self.assertEqual(module_calls(3000), sum(i + 1 for i in range(3000)))
        old = GM.f
        GM.f = lambda v: v * 2
        try:
            self.assertEqual(module_calls(3000), sum(i * 2 for i in range(3000)))
        finally:
            GM.f = old
        del GM.f
        try:
            with self.assertRaises(AttributeError):
                module_calls(10)
        finally:
            GM.f = old

    def test_global_slot_reads(self):
        self.assertEqual(slot_reads(5000), 5000 * 7)
        GP.x = 10
        try:
            self.assertEqual(slot_reads(5000), 5000 * 14)
            del GP.y
            with self.assertRaises(AttributeError):
                slot_reads(10)
        finally:
            GP.x, GP.y = 3, 4

    def test_identity_of_repeated_reads(self):
        class Holder:
            pass

        h = Holder()
        h.big = 10**30
        h.items = [1]
        g = {"h": h}
        exec(
            "def collect(n):\n"
            "    out = []\n"
            "    for _ in range(n):\n"
            "        out.append(h.big)\n"
            "        out.append(h.items)\n"
            "    return out\n",
            g,
        )
        out = g["collect"](5000)
        self.assertTrue(all(v is h.big for v in out[0::2]))
        self.assertTrue(all(v is h.items for v in out[1::2]))
        h.items = [2]
        out = g["collect"](10)
        self.assertEqual(out[1], [2])


if __name__ == "__main__":
    unittest.main()
