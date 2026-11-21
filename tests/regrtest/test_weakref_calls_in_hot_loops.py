"""Calling weak references in hot loops.

`r()` has a leaf half that reads the referent in place; it must answer the
live referent, `None` once the referent died (even mid-loop), and leave a
subclass's own `__call__`, callbacks and proxies to the full path.
"""

import gc
import unittest
import weakref


class Obj:
    pass


class WeakrefCallTests(unittest.TestCase):
    ROUNDS = 5000

    def test_live_and_dead(self):
        o = Obj()
        r = weakref.ref(o)
        for _ in range(self.ROUNDS):
            self.assertIs(r(), o)
        del o
        gc.collect()
        for _ in range(self.ROUNDS):
            self.assertIsNone(r())

    def test_dies_inside_loop(self):
        objs = [Obj() for _ in range(50)]
        refs = [weakref.ref(x) for x in objs]
        seen = []
        for i in range(len(refs)):
            seen.append(refs[i]() is not None)
            if i == 20:
                del objs[:]
                gc.collect()
        self.assertEqual(seen, [True] * 21 + [False] * 29)

    def test_subclass(self):
        class R(weakref.ref):
            pass

        class Custom(weakref.ref):
            def __call__(self):
                return ("custom", super().__call__())

        o = Obj()
        r, c = R(o), Custom(o)
        for _ in range(self.ROUNDS):
            self.assertIs(r(), o)
            self.assertEqual(c(), ("custom", o))

    def test_callback_still_runs(self):
        calls = []
        o = Obj()
        r = weakref.ref(o, calls.append)
        for _ in range(self.ROUNDS):
            r()
        del o
        gc.collect()
        self.assertEqual(calls, [r])
        self.assertIsNone(r())

    def test_arguments_rejected(self):
        o = Obj()
        r = weakref.ref(o)
        for _ in range(3):
            with self.assertRaises(TypeError):
                r(1)

    def test_weak_value_dictionary(self):
        o = Obj()
        d = weakref.WeakValueDictionary({1: o})
        for _ in range(self.ROUNDS):
            self.assertIs(d[1], o)
        del o
        gc.collect()
        with self.assertRaises(KeyError):
            d[1]


if __name__ == "__main__":
    unittest.main()
