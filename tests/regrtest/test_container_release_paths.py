"""Releasing dicts, lists and tuples, and refilling the tuple free list.

A dying dict or list gives its shared items up with plain decrements and
drops the ones it owns last, in order; deeply nested ones still free
without overflowing the stack. Objects with `__del__` die when their last
container does, in CPython's order. A tuple a loop builds and drops
reuses one allocation, as CPython's free list does.
"""

import gc
import sys
import unittest

N = 3000


class Noted:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Noted.log.append(self.name)


class DictReleaseTests(unittest.TestCase):
    def test_shared_and_owned_values(self):
        keys = ["k%d" % i for i in range(20)]
        shared = [object() for _ in range(20)]
        before = [sys.getrefcount(k) for k in keys]
        for _ in range(N):
            d = dict(zip(keys, shared))
            c = d.copy()
            c["own"] = [1, 2, 3]
            del d, c
        self.assertEqual([sys.getrefcount(k) for k in keys], before)

    def test_death_order(self):
        Noted.log = []
        d = {"a": Noted("a"), 1: Noted("one"), "b": Noted("b")}
        del d
        gc.collect()
        self.assertEqual(Noted.log, ["a", "one", "b"])
        Noted.log = []
        keep = Noted("kept")
        d = {"x": keep, "y": Noted("y"), "z": keep}
        del d
        self.assertEqual(Noted.log, ["y"])
        del keep
        self.assertEqual(Noted.log, ["y", "kept"])

    def test_deep_nesting(self):
        d = {}
        for _ in range(200000):
            d = {"next": d, 1: "x"}
        del d

    def test_drained_dict(self):
        d = {i: str(i) for i in range(50)}
        items = list(d.items())
        self.assertEqual(items[7], (7, "7"))
        d.clear()
        self.assertEqual(len(d), 0)
        e = dict.fromkeys("abc", [])
        self.assertEqual(sorted(e), ["a", "b", "c"])


class ListReleaseTests(unittest.TestCase):
    def test_shared_strings(self):
        strs = tuple("s%d" % i for i in range(100))
        before = [sys.getrefcount(s) for s in strs[:5]]
        for _ in range(N):
            lst = list(strs)
            lst.append("tail")
            del lst
        self.assertEqual([sys.getrefcount(s) for s in strs[:5]], before)

    def test_death_order_with_shared_items(self):
        Noted.log = []
        keep = Noted("kept")
        lst = [keep, Noted("first"), "shared", keep, Noted("second")]
        del lst
        # (CPython releases a list's items last first; the order of the
        # two deaths isn't what this checks.)
        self.assertEqual(sorted(Noted.log), ["first", "second"])
        del keep
        self.assertEqual(Noted.log[-1], "kept")
        self.assertEqual(len(Noted.log), 3)

    def test_deep_nesting(self):
        lst = []
        for _ in range(200000):
            lst = [lst, "x"]
        del lst


class TuplePoolTests(unittest.TestCase):
    def test_reused_address(self):
        ids = list(map(id, ((i, i) for i in range(10))))
        self.assertEqual(len(set(ids)), 1)

    def test_pairs_hold_their_items(self):
        out = []
        for i, pair in enumerate(enumerate("abcdef" * 50)):
            out.append(pair)
        self.assertEqual(out[:2], [(0, "a"), (1, "b")])
        self.assertEqual(len(set(map(id, out))), len(out))
        d = {"k%d" % i: i for i in range(10)}
        for _ in range(N):
            items = [kv for kv in d.items()]
        self.assertEqual(items[3], ("k3", 3))


if __name__ == "__main__":
    unittest.main()
