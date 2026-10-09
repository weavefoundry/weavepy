"""Plain class values read through the class, under any metaclass.

`Color.RED`, `Cls.CONST` and `Cls.method` read the value stored on the
class's MRO as `type` reads it. A metaclass that defines the name or
replaces `__getattribute__`, a rebound or deleted class attribute, and
values that bind (a static method, a property, a descriptor instance)
must resolve as the ordinary lookup does.
"""

import enum
import unittest


class Color(enum.Enum):
    RED = 1
    GREEN = 2


class Plain:
    CONST = "c"

    def method(self):
        return 1

    @staticmethod
    def static():
        return 2


class Meta(type):
    @property
    def CONST(cls):
        return "meta"


class Shadowed(metaclass=Meta):
    CONST = "class"


class Logging(type):
    seen = []

    def __getattribute__(cls, name):
        Logging.seen.append(name)
        return super().__getattribute__(name)


class Watched(metaclass=Logging):
    VALUE = 3


class Descr:
    def __get__(self, obj, owner):
        return "described"


class WithDescr:
    D = Descr()


class ClassValueReadTest(unittest.TestCase):
    def test_enum_members(self):
        for _ in range(2000):
            self.assertIs(Color.RED, Color(1))
            self.assertEqual(Color.GREEN.value, 2)

    def test_plain_values(self):
        for _ in range(2000):
            self.assertEqual(Plain.CONST, "c")
            self.assertEqual(Plain.method(Plain()), 1)
            self.assertEqual(Plain.static(), 2)
            self.assertEqual(WithDescr.D, "described")

    def test_metaclass_rules(self):
        for _ in range(2000):
            self.assertEqual(Shadowed.CONST, "meta")
        Logging.seen.clear()
        for _ in range(100):
            self.assertEqual(Watched.VALUE, 3)
        self.assertEqual(Logging.seen.count("VALUE"), 100)

    def test_rebinding(self):
        class C:
            X = 1

        seen = []
        for i in range(2000):
            if i == 1000:
                C.X = 2
            seen.append(C.X)
        self.assertEqual(seen[999], 1)
        self.assertEqual(seen[1000], 2)
        del C.X
        with self.assertRaises(AttributeError):
            C.X


if __name__ == "__main__":
    unittest.main()
