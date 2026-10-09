"""Property reads served by a getter the read's site remembers.

A warm `obj.prop` site evaluates the property's getter in place, and a
getter that reads another property evaluates that one too. Rebinding the
property's getter, the getter's code, or the class must never call a
released function, and a getter with locals, a `try` statement, or a
nested read must return what the ordinary call would.
"""

import unittest


class Point:
    __slots__ = ("_x", "_y")

    def __init__(self, x, y):
        self._x = x
        self._y = y

    @property
    def x(self):
        return self._x

    @property
    def y(self):
        return self._y

    @property
    def norm1(self):
        a = self.x
        b = self.y
        return abs(a) + abs(b)

    @property
    def guarded(self):
        try:
            return self._x
        except AttributeError:
            return None


class Box:
    def __init__(self, items):
        self.items = items

    @property
    def last(self):
        items = self.items
        return items[-1]

    @property
    def last_twice(self):
        v = self.last
        return v + v


def read_x(p):
    return p.x


def total(points, n):
    s = 0
    for _ in range(n):
        for p in points:
            s += p.norm1
    return s


class PropertySiteGetterTest(unittest.TestCase):
    def test_plain_and_nested(self):
        pts = [Point(i, -i) for i in range(10)]
        self.assertEqual(total(pts, 300), 300 * sum(2 * i for i in range(10)))
        b = Box([1, 2, 3])
        out = 0
        for _ in range(3000):
            out += b.last_twice
        self.assertEqual(out, 3000 * 6)

    def test_handler_getter(self):
        p = Point(4, 5)
        for _ in range(3000):
            self.assertEqual(p.guarded, 4)
        del p._x
        self.assertIsNone(p.guarded)

    def test_getter_reinit(self):
        class C:
            def __init__(self):
                self.v = 1

            def get_v(self):
                return self.v

            def get_double(self):
                return self.v * 2

            prop = property(get_v)

        c = C()

        def loop():
            s = 0
            for _ in range(2000):
                s += c.prop
            return s

        self.assertEqual(loop(), 2000)
        prop = C.__dict__["prop"]
        prop.__init__(C.get_double)
        # (A specialized site may keep the old getter, as CPython's does.)
        self.assertIn(loop(), (2000, 4000))
        self.assertEqual(prop.__get__(c), 2)

    def test_code_swap(self):
        class C:
            def __init__(self):
                self.v = 3

            @property
            def p(self):
                return self.v

        def other(self):
            return self.v + 100

        c = C()
        for _ in range(2000):
            self.assertEqual(c.p, 3)
        C.__dict__["p"].fget.__code__ = other.__code__
        for _ in range(10):
            self.assertEqual(c.p, 103)

    def test_class_change(self):
        class C:
            def __init__(self):
                self.v = 7

            @property
            def p(self):
                return self.v

        c = C()
        for _ in range(2000):
            self.assertEqual(c.p, 7)
        C.p = property(lambda self: -self.v)
        self.assertEqual(c.p, -7)
        del C.p
        with self.assertRaises(AttributeError):
            c.p

    def test_nested_read_sees_change(self):
        b = Box([1, 2])
        for _ in range(2000):
            self.assertEqual(b.last_twice, 4)
        original = Box.__dict__["last"]
        Box.last = property(lambda self: self.items[0])
        try:
            self.assertEqual(b.last_twice, 2)
        finally:
            Box.last = original

    def test_getter_error_falls_back_to_getattr(self):
        class C:
            @property
            def p(self):
                return self.missing

            def __getattr__(self, name):
                return name

        c = C()
        for _ in range(2000):
            self.assertEqual(c.p, "missing")


if __name__ == "__main__":
    unittest.main()
