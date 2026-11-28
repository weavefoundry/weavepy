"""Builtin type constructors called from small functions and getters.

A function or property getter whose body only reads and calls
(`return list(self.items)`, `return str(self.n)`) is evaluated in place,
and keeps being once its plan is compiled, a builtin type's constructor
included. Results are fresh objects where CPython's are, arguments the
constructor can't take fall back to the ordinary call, and reads from a
generator's steps (`sum(x.prop for x in xs)`) agree.
"""

import unittest

N = 30000


class Box:
    def __init__(self, items):
        self.items = items
        self.n = len(items)

    @property
    def copy(self):
        return list(self.items)

    @property
    def text(self):
        return str(self.n)

    @property
    def frozen(self):
        return tuple(self.items)

    def as_int(self, s):
        return int(s)

    def fresh(self):
        return dict()


class LeafConstructorTests(unittest.TestCase):
    def test_results_are_fresh_copies(self):
        b = Box([1, 2])
        seen = []
        for i in range(N):
            c = b.copy
            if i % 5000 == 0:
                seen.append(c)
        self.assertEqual(c, [1, 2])
        self.assertIsNot(c, b.items)
        c.append(3)
        self.assertEqual(b.items, [1, 2])
        self.assertEqual(len({id(x) for x in seen}), len(seen))

    def test_other_constructors(self):
        b = Box([1, 2, 3])
        for _ in range(N):
            t, f, k, d = b.text, b.frozen, b.as_int("42"), b.fresh()
        self.assertEqual((t, f, k, d), ("3", (1, 2, 3), 42, {}))
        d["x"] = 1
        self.assertEqual(b.fresh(), {})

    def test_arguments_the_constructor_declines(self):
        b = Box([1])
        out = []
        for i in range(N):
            s = "12" if i % 2 else "0x1"
            try:
                out.append(b.as_int(s))
            except ValueError:
                out.append(None)
        self.assertEqual(out[:2], [None, 12])
        b.items = (x for x in range(3))
        self.assertEqual(b.copy, [0, 1, 2])

    def test_generator_steps(self):
        boxes = [Box([i]) for i in range(4)]
        for _ in range(N // 4):
            r = sum((x.copy for x in boxes), [])
            s = "".join(x.text for x in boxes)
        self.assertEqual(r, [0, 1, 2, 3])
        self.assertEqual(s, "1111")

    def test_generator_steps_with_activation_getters(self):
        def helper(x):
            return [x, x]

        class Node:
            def __init__(self, v):
                self.v = v

            @property
            def pair(self):
                return helper(self.v)

        nodes = [Node(i) for i in range(3)]
        for _ in range(N // 3):
            r = sum((n.pair for n in nodes), [])
        self.assertEqual(r, [0, 0, 1, 1, 2, 2])

    def test_subclass_items(self):
        class L(list):
            pass

        b = Box(L([5]))
        for _ in range(N):
            c = b.copy
        self.assertIs(type(c), list)
        self.assertEqual(c, [5])


if __name__ == "__main__":
    unittest.main()
