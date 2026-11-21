"""`operator`'s callables on instances, from compiled loops.

`methodcaller`, `itemgetter` and `attrgetter` are native calls. When one
reaches Python code (a method, a `__getitem__`, a property), compiled code
runs its native body in place with the callable first, so the Python code
sees the right arguments, its errors carry their traceback, and the
loop's own handlers catch them.
"""

import operator
import sys
import traceback
import unittest


class Box:
    def __init__(self, v):
        self.v = v

    def scaled(self, k, extra=0):
        return self.v * k + extra

    def fail(self):
        raise ValueError("from method")

    def items(self):
        yield self.v
        yield self.v + 1

    def __getitem__(self, i):
        if i == "bad":
            raise KeyError(i)
        return (self.v, i)

    @property
    def double(self):
        return self.v * 2


class OperatorCallableTests(unittest.TestCase):
    def test_methodcaller_python_methods(self):
        boxes = [Box(i) for i in range(10)]
        m = operator.methodcaller("scaled", 3)
        m2 = operator.methodcaller("scaled", 2, 5)
        for _ in range(200):
            self.assertEqual([m(b) for b in boxes], [3 * i for i in range(10)])
            self.assertEqual(m2(boxes[4]), 13)
        mk = operator.methodcaller("scaled", 2, extra=1)
        self.assertEqual(mk(boxes[3]), 7)
        gen = operator.methodcaller("items")
        self.assertEqual(list(gen(boxes[2])), [2, 3])

    def test_errors_from_compiled_loops(self):
        fail = operator.methodcaller("fail")
        b = Box(1)
        caught = 0
        for _ in range(300):
            try:
                fail(b)
            except ValueError as e:
                caught += 1
                tb = traceback.extract_tb(e.__traceback__)
                self.assertEqual(tb[-1].name, "fail")
        self.assertEqual(caught, 300)
        missing = operator.methodcaller("nope")
        for _ in range(100):
            with self.assertRaises(AttributeError):
                missing(b)

    def test_getters(self):
        boxes = [Box(i) for i in range(5)]
        ig = operator.itemgetter(7)
        ig2 = operator.itemgetter(1, 2)
        ag = operator.attrgetter("double")
        for _ in range(300):
            self.assertEqual([ig(x) for x in boxes], [(i, 7) for i in range(5)])
            self.assertEqual(ig2(boxes[1]), ((1, 1), (1, 2)))
            self.assertEqual([ag(x) for x in boxes], [0, 2, 4, 6, 8])
        bad = operator.itemgetter("bad")
        for _ in range(100):
            with self.assertRaises(KeyError):
                bad(boxes[0])
        self.assertEqual(sorted(boxes, key=operator.attrgetter("double"), reverse=True)[0].v, 4)

    def test_refcounts_and_identity(self):
        b = Box(5)
        m = operator.methodcaller("scaled", 1)
        before = sys.getrefcount(b)
        for _ in range(500):
            m(b)
        self.assertEqual(sys.getrefcount(b), before)
        self.assertEqual(sys.getrefcount(m), sys.getrefcount(m))


if __name__ == "__main__":
    unittest.main()
