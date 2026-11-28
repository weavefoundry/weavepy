"""A `**kwargs` dict that's only read (a truth test, `in`, a loop over its
keys, `len`, `get` and `pop`) isn't registered with the collector; one that
escapes into a cycle still is, and is collected."""

import gc
import unittest


def reads(a, **kw):
    n = len(kw)
    if kw:
        n += 100
    if "x" in kw:
        n += kw.get("x", 0)
    for k in kw:
        n += len(k)
    n += kw.pop("y", 0)
    return n


def escapes(**kw):
    kw["box"].append(kw)
    return kw["box"]


def view_escapes(**kw):
    return kw.items()


class Box(list):
    pass


class KwargsReads(unittest.TestCase):
    def test_reads(self):
        for _ in range(500):
            self.assertEqual(reads(1), 0)
            self.assertEqual(reads(1, x=5), 1 + 100 + 5 + 1)
            self.assertEqual(reads(1, x=5, y=7), 2 + 100 + 5 + 2 + 7)
            self.assertEqual(reads(1, zz=1), 1 + 100 + 2)

    def test_escaping_dict_collected(self):
        gc.collect()
        for _ in range(300):
            escapes(box=Box())
        gc.collect()
        self.assertFalse([o for o in gc.get_objects() if type(o) is Box])

    def test_view_keeps_dict(self):
        for _ in range(200):
            v = view_escapes(a=1, b=2)
        self.assertEqual(sorted(v), [("a", 1), ("b", 2)])

    def test_fresh_dict_each_call(self):
        def keep(**kw):
            return kw
        a = keep(x=1)
        b = keep(y=2)
        self.assertIsNot(a, b)
        self.assertEqual((a, b), ({"x": 1}, {"y": 2}))


if __name__ == "__main__":
    unittest.main()
