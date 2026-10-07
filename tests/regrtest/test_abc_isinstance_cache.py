"""`isinstance` against ABCs answered from the ABC's positive cache.

A class whose subclass check succeeded once stays an answer of `True`,
read without calling `__instancecheck__`. A class only registered later,
a cleared cache, a replaced `__instancecheck__`, an instance whose
`__class__` reports another class, and a subclass hook that rejects must
still go through the full check.
"""

import abc
import unittest


class Shape(abc.ABC):
    pass


class Rect(Shape):
    pass


class Other:
    pass


class AbcIsinstanceCacheTest(unittest.TestCase):
    def test_subclass_and_tuple(self):
        r = Rect()
        for _ in range(2000):
            self.assertTrue(isinstance(r, Shape))
            self.assertTrue(isinstance(r, (int, Shape)))
            self.assertFalse(isinstance(Other(), Shape))

    def test_registration_and_cache_clear(self):
        class Base(abc.ABC):
            pass

        o = Other()
        for _ in range(500):
            self.assertFalse(isinstance(o, Base))
        Base.register(Other)
        self.assertTrue(isinstance(o, Base))
        Base._abc_caches_clear()
        self.assertTrue(isinstance(o, Base))

    def test_custom_metaclass_hook(self):
        class Meta(abc.ABCMeta):
            def __instancecheck__(cls, instance):
                return False

        class Picky(metaclass=Meta):
            pass

        class Child(Picky):
            pass

        c = Child()
        for _ in range(500):
            self.assertFalse(isinstance(c, Picky))
        self.assertTrue(isinstance(c, Child))

    def test_class_attribute_override(self):
        class Liar(Shape):
            @property
            def __class__(self):
                return Other

        x = Liar()
        for _ in range(500):
            self.assertTrue(isinstance(x, Shape))
            self.assertTrue(isinstance(x, Other))

    def test_subclass_hook_rejects(self):
        class Gate(abc.ABC):
            @classmethod
            def __subclasshook__(cls, sub):
                return False

        class Inside(Gate):
            pass

        for _ in range(500):
            self.assertTrue(isinstance(Inside(), Inside))
            self.assertFalse(isinstance(Inside(), Gate))


if __name__ == "__main__":
    unittest.main()
