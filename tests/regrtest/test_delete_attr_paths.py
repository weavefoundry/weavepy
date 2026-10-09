"""`del obj.attr` on the fast path and every case it must leave alone.

An ordinary instance's own attribute is deleted directly. A missing one
raises AttributeError; a class with its own `__delattr__` (even one added
later), a property's deleter, a `__slots__` member, an exception's core
attributes, and a name the class supplies all keep their behavior.
"""

import unittest


class Plain:
    pass


class Logged:
    def __init__(self):
        self.log = []

    def __delattr__(self, name):
        self.log.append(name)
        object.__delattr__(self, name)


class WithProp:
    def __init__(self):
        self.deleted = 0

    @property
    def p(self):
        return 1

    @p.deleter
    def p(self):
        self.deleted += 1


class Slotted:
    __slots__ = ("a",)


class Released:
    count = 0

    def __del__(self):
        Released.count += 1


class DeleteAttrTest(unittest.TestCase):
    def test_own_attributes(self):
        for _ in range(500):
            o = Plain()
            o.a, o.b, o.c = 1, 2, 3
            del o.b
            self.assertEqual(vars(o), {"a": 1, "c": 3})
            del o.a, o.c
            self.assertEqual(vars(o), {})
            o.b = 4
            self.assertEqual(o.b, 4)

    def test_missing_raises(self):
        o = Plain()
        for _ in range(300):
            o.x = 1
            del o.x
            with self.assertRaises(AttributeError):
                del o.x

    def test_custom_delattr(self):
        for _ in range(300):
            o = Logged()
            o.x = 1
            del o.x
            self.assertEqual(o.log, ["x"])

    def test_delattr_added_later(self):
        class Late:
            pass

        o = Late()
        for _ in range(300):
            o.x = 1
            del o.x
        calls = []
        Late.__delattr__ = lambda self, name: calls.append(name)
        o.x = 1
        del o.x
        self.assertEqual(calls, ["x"])
        self.assertEqual(o.x, 1)

    def test_property_deleter(self):
        o = WithProp()
        for _ in range(300):
            del o.p
        self.assertEqual(o.deleted, 300)

    def test_slots(self):
        s = Slotted()
        for _ in range(300):
            s.a = 1
            del s.a
            with self.assertRaises(AttributeError):
                s.a

    def test_class_attribute_not_instance(self):
        class C:
            def m(self):
                return 1

        c = C()
        for _ in range(100):
            with self.assertRaises(AttributeError):
                del c.m
        self.assertEqual(c.m(), 1)

    def test_exception_attributes(self):
        e = ValueError("x")
        with self.assertRaises(TypeError):
            del e.args
        e.note = 1
        del e.note
        self.assertFalse(hasattr(e, "note"))

    def test_value_released_at_once(self):
        Released.count = 0
        o = Plain()
        for i in range(200):
            o.r = Released()
            del o.r
            self.assertEqual(Released.count, i + 1)


if __name__ == "__main__":
    unittest.main()
