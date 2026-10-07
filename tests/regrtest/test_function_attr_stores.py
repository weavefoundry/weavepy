"""Attribute stores on functions, and their release of the old value.

`wrapper.calls += 1` reads and writes the function's `__dict__` in
place. A replaced value whose last reference goes must be finalized at
once, in a compiled loop too, and dunder names, which the function type
defines, must keep their own behavior.
"""

import functools
import unittest


def counted(fn):
    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        wrapper.calls += 1
        return fn(*args, **kwargs)

    wrapper.calls = 0
    return wrapper


@counted
def area(w, h=1, *, scale=1.0):
    return w * h * scale


class Noisy:
    log = []

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        Noisy.log.append(self.tag)


class FunctionAttrStoreTest(unittest.TestCase):
    def test_counter(self):
        before = area.calls
        for i in range(3000):
            area(i, h=2, scale=0.5)
        self.assertEqual(area.calls - before, 3000)

    def test_replaced_value_finalized(self):
        def f():
            pass

        Noisy.log.clear()
        for i in range(200):
            f.holder = Noisy(i)
            if i:
                self.assertEqual(Noisy.log[-1], i - 1)
        del f.holder
        self.assertEqual(Noisy.log[-1], 199)

    def test_replaced_attribute_finalized_in_loop(self):
        def f():
            pass

        Noisy.log.clear()
        late = []
        for i in range(300):
            f.holder = Noisy(i)
            if i and (not Noisy.log or Noisy.log[-1] != i - 1):
                late.append(i)
        self.assertEqual(late, [])

    def test_dunder_names(self):
        def g():
            "doc"

        for i in range(500):
            g.__doc__ = "d%d" % i
            g.__name__ = "n%d" % i
        self.assertEqual(g.__doc__, "d499")
        self.assertEqual(g.__name__, "n499")
        with self.assertRaises(TypeError):
            g.__code__ = 1


if __name__ == "__main__":
    unittest.main()
