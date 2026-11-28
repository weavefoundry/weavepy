"""A class's own `__setattr__`, found once per class version.

The inline store finds a class's Python `__setattr__` (and a native one)
through a cache the class's version guards: replacing the hook on the
class, or on a base the class inherits it from, or deleting it, takes
effect at the next store.
"""

import threading
import unittest


class SetattrHookTests(unittest.TestCase):
    def test_replaced_on_class(self):
        log = []

        class A:
            def __setattr__(self, k, v):
                log.append(("one", k))
                object.__setattr__(self, k, v)

        a = A()
        for i in range(300):
            a.x = i
        self.assertEqual(len(log), 300)

        def two(self, k, v):
            log.append(("two", k))
            object.__setattr__(self, k, v * 2)

        A.__setattr__ = two
        for i in range(300):
            a.y = i
        self.assertEqual(log[-1], ("two", "y"))
        self.assertEqual(a.y, 598)
        del A.__setattr__
        a.z = 5
        self.assertEqual(log[-1], ("two", "y"))
        self.assertEqual(a.z, 5)

    def test_replaced_on_base(self):
        class Base:
            def __setattr__(self, k, v):
                self.__dict__[k] = ("base", v)

        class Sub(Base):
            pass

        s = Sub()
        for i in range(300):
            s.v = i
        self.assertEqual(s.v, ("base", 299))

        def other(self, k, v):
            self.__dict__[k] = ("other", v)

        Base.__setattr__ = other
        for i in range(300):
            s.v = i
        self.assertEqual(s.v, ("other", 299))

    def test_native_hook(self):
        loc = threading.local()
        for i in range(300):
            loc.n = i
        self.assertEqual(loc.n, 299)

        class L(threading.local):
            pass

        l = L()
        for i in range(300):
            l.n = i
        self.assertEqual(l.n, 299)

    def test_raising_hook(self):
        class Frozen:
            def __setattr__(self, k, v):
                raise AttributeError(k)

        f = Frozen()
        for i in range(300):
            with self.assertRaises(AttributeError):
                f.x = i


if __name__ == "__main__":
    unittest.main()
