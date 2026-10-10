"""Instances discarded by a statement (`C(...)` whose value is unused).

A plain instance that dies as a compiled loop pops it goes back to the
instance pool. One owing a finalizer, one with a weak reference or one
another owner still holds must not be pooled, and a pooled instance
reused for another class must carry nothing of its last tenant.
"""

import gc
import unittest
import weakref


class Plain:
    def __init__(self, a):
        self.a = a
        self.b = "b"


class Slotted:
    __slots__ = ("x", "y")

    def __init__(self, x):
        self.x = x


class Final:
    log = []

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        Final.log.append(self.tag)


class Weak:
    __slots__ = ("v", "__weakref__")

    def __init__(self, v):
        self.v = v


class PoolTest(unittest.TestCase):
    def test_discarded_in_a_loop(self):
        for i in range(3000):
            Plain(i)
            Slotted(i)
        p = Plain(1)
        self.assertEqual(vars(p), {"a": 1, "b": "b"})
        s = Slotted.__new__(Slotted)
        self.assertFalse(hasattr(s, "x"))
        self.assertFalse(hasattr(s, "y"))
        e = Plain.__new__(Plain)
        self.assertEqual(vars(e), {})

    def test_finalizer_runs_at_the_statement(self):
        Final.log.clear()
        for i in range(500):
            Final(i)
            self.assertEqual(Final.log[-1], i)
        self.assertEqual(len(Final.log), 500)

    def test_weakref_callback(self):
        fired = []
        refs = []
        for i in range(300):
            w = Weak(i)
            refs.append(weakref.ref(w, lambda r, i=i: fired.append(i)))
            del w
        self.assertEqual(fired, list(range(300)))
        self.assertTrue(all(r() is None for r in refs))

    def test_shared_owner_survives(self):
        keep = []
        for i in range(1000):
            keep.append(Plain(i))
            Plain(-i)
        self.assertEqual([k.a for k in keep], list(range(1000)))
        gc.collect()
        self.assertEqual(keep[500].b, "b")


if __name__ == "__main__":
    unittest.main()
