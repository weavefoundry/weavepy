"""`__build_class__` reads the class body's cells after the body has run.

A finished frame releases its cells, so the audit of `__classcell__` and the
re-pointing of the `__classdict__` cell need a handle of their own. The results
below are CPython's.
"""

import unittest


class ClassCellAudit(unittest.TestCase):
    def test_missing_classcell_raises(self):
        class Meta(type):
            def __new__(cls, name, bases, namespace):
                namespace.pop("__classcell__", None)
                return super().__new__(cls, name, bases, namespace)

        class WithoutClassRef(metaclass=Meta):
            pass

        with self.assertRaisesRegex(RuntimeError, "__class__ not set.*__classcell__ propagated"):
            class WithClassRef(metaclass=Meta):
                def f(self):
                    return __class__

    def test_wrong_cell_raises(self):
        class Meta(type):
            def __new__(cls, name, bases, namespace):
                cls = super().__new__(cls, name, bases, namespace.copy())
                return cls

        class Second(type):
            def __new__(cls, name, bases, namespace):
                super().__new__(cls, name + "2", bases, dict(namespace))
                return super().__new__(cls, name, (), {})

        with self.assertRaises(TypeError):
            class A(metaclass=Second):
                def f(self):
                    return __class__

    def test_propagated_classcell_is_set(self):
        class Meta(type):
            def __new__(cls, name, bases, namespace):
                return super().__new__(cls, name, bases, namespace)

        class B(metaclass=Meta):
            def f(self):
                return __class__

        self.assertIs(B().f(), B)

    def test_classdict_sees_later_changes(self):
        class C:
            T = int
            type Alias = T

        C.T = float
        self.assertIs(C.Alias.__value__, float)


if __name__ == "__main__":
    unittest.main()
