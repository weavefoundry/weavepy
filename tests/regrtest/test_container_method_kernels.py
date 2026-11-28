"""Container methods that compiled code runs in place.

`dict.pop`, `dict.copy`, `set.copy` and `list.remove` run as kernels or
leaf bodies when nothing they touch can run Python code, and decline to
the full call otherwise. These loops check both sides of that line.
"""

import unittest

N = 3000


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v


class Loose:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return getattr(other, "v", other) == self.v

    def __repr__(self):
        return f"Loose({self.v})"


class DictPopCopyTests(unittest.TestCase):
    def test_pop(self):
        class DD(dict):
            pass

        for i in range(N):
            d = {"a": 1, "b": 2, 3: 4, Key(1): 5}
            r = (d.pop("a"), d.pop("zz", None), d.pop(3), d.pop(Key(1)),
                 d.pop(2.0, "f"), list(d))
            dd = DD(x=i)
            r2 = (dd.pop("x"), dd)
        self.assertEqual(r, (1, None, 4, 5, "f", ["b"]))
        self.assertEqual(r2, (N - 1, {}))
        with self.assertRaises(KeyError) as cm:
            {}.pop("missing")
        self.assertEqual(cm.exception.args, ("missing",))
        with self.assertRaises(TypeError) as cm:
            {}.pop()
        self.assertEqual(str(cm.exception), "pop expected at least 1 argument, got 0")

    def test_copy(self):
        class DD(dict):
            pass

        d = {"a": 1}
        s = {1, 2}
        for i in range(N):
            c = d.copy()
            c["n"] = i
            t = s.copy()
            t.add(3)
            sub = DD(x=1).copy()
        self.assertEqual((d, c, s, t), ({"a": 1}, {"a": 1, "n": N - 1}, {1, 2}, {1, 2, 3}))
        self.assertIs(type(sub), dict)
        with self.assertRaises(TypeError) as cm:
            {}.copy(1)
        self.assertEqual(str(cm.exception), "dict.copy() takes no arguments (1 given)")


class ListRemoveTests(unittest.TestCase):
    def test_native_and_python_equality(self):
        for i in range(N):
            items = [1, "a", 2.0, 3, None, Loose(5)]
            items.remove(3)
            items.remove("a")
            items.remove(2)
            items.remove(None)
            items.remove(5)
        self.assertEqual(items, [1])
        mixed = [Loose(1), 2]
        mixed.remove(2)
        self.assertEqual(repr(mixed), "[Loose(1)]")
        with self.assertRaises(ValueError) as cm:
            [1].remove(99)
        self.assertEqual(str(cm.exception), "list.remove(x): x not in list")

    def test_removed_item_finalizes_promptly(self):
        events = []

        class D:
            def __del__(self):
                events.append("del")

        for i in range(N):
            items = [D() if i == N - 1 else 0, 7]
            items.remove(items[0])
            if i == N - 1:
                events.append("after")
        self.assertEqual(events, ["del", "after"])


if __name__ == "__main__":
    unittest.main()
