"""`cls.__qualname__` and a bound method's attributes, compiled and not.

A site that reads a class's `__name__`, `__qualname__` or `__module__`
remembers the string for the class's version; assigning `__qualname__`
moves the version, so the site reads the new name. A bound method's
`__self__`, `__func__`, `__name__`, `__qualname__` and `__doc__` read
directly: names a decorator patched onto the function, builtin methods'
qualified names, and metaclasses that answer the names themselves behave
as before.
"""

import functools
import unittest

N = 3000


class Plain:
    """plain doc"""

    def method(self):
        """method doc"""


class Meta(type):
    @property
    def __module__(cls):
        return "meta-module"


class ByMeta(metaclass=Meta):
    pass


def renamed(f):
    @functools.wraps(f)
    def inner(*a):
        return f(*a)

    inner.__name__ = "patched"
    return inner


class Wrapped:
    @renamed
    def original(self):
        pass


def qual(c):
    return c.__qualname__


class ClassNameTests(unittest.TestCase):
    def test_qualname_assignment(self):
        class C:
            pass

        out = []
        for i in range(N):
            if i == N // 2:
                C.__qualname__ = "Outer.C"
            out.append((qual(C), C.__name__))
        self.assertEqual(out[0][0], "ClassNameTests.test_qualname_assignment.<locals>.C")
        self.assertEqual(out[-1], ("Outer.C", "C"))
        self.assertIs(qual(C), qual(C))

    def test_name_assignment_and_mixed_classes(self):
        class A:
            pass

        class B:
            pass

        out = []
        for i in range(N):
            if i == N // 2:
                A.__name__ = "Renamed"
            out.append(((A, B)[i % 2]).__name__)
        self.assertEqual(out[:2], ["A", "B"])
        self.assertEqual(out[-2:], ["Renamed", "B"])

    def test_metaclass_names(self):
        for _ in range(N):
            r = (ByMeta.__module__, ByMeta.__name__, qual(int), qual(Plain))
        self.assertEqual(r, ("meta-module", "ByMeta", "int", "Plain"))


class BoundMethodAttrTests(unittest.TestCase):
    def test_python_method(self):
        p = Plain()
        m = p.method
        for _ in range(N):
            r = (m.__self__, m.__func__, m.__name__, m.__qualname__, m.__doc__)
        self.assertEqual(r, (p, Plain.method, "method", "Plain.method", "method doc"))

    def test_patched_function_names(self):
        m = Wrapped().original
        for _ in range(N):
            r = (m.__name__, m.__qualname__)
        self.assertEqual(r, ("patched", "Wrapped.original"))

    def test_builtin_methods(self):
        lst = []
        app = lst.append
        fk = dict.fromkeys
        for _ in range(N):
            r = (app.__self__ is lst, app.__name__, app.__qualname__, fk.__qualname__)
        self.assertEqual(r, (True, "append", "list.append", "dict.fromkeys"))

    def test_classmethod_and_mixed_receivers(self):
        class K:
            @classmethod
            def make(cls):
                return cls

            def inst(self):
                pass

        k = K()
        ms = [K.make, k.inst, len, Plain().method]
        out = []
        for i in range(N):
            m = ms[i % 4]
            out.append(m.__name__)
        self.assertEqual(out[:4], ["make", "inst", "len", "method"])
        self.assertIs(K.make.__self__, K)
        with self.assertRaises(AttributeError):
            k.inst.no_such_attr


if __name__ == "__main__":
    unittest.main()
