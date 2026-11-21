"""itemgetter on built-in subclasses, methodcaller's direct method call, and
`str()` / `repr()` / `f'{x!r}'` of instances with Python dunders.

itemgetter's fast half reads a subclass's payload (a named tuple) when its
`__getitem__` is still the base's own; methodcaller calls a plain
instance's Python method with the instance first; `!r` and `str()` call a
Python `__repr__` directly. Each must keep the semantics of the full path.
"""

import collections
import operator
import unittest


class Overrides(tuple):
    def __getitem__(self, i):
        return ("over", i)


class MissingDict(dict):
    def __missing__(self, key):
        return ("missing", key)


class GetitemDict(dict):
    def __getitem__(self, key):
        return ("own", key)


class ItemgetterTests(unittest.TestCase):
    def test_named_tuple(self):
        NT = collections.namedtuple("NT", "a b c")
        t = NT(1, [2], "c")
        g = operator.itemgetter(1)
        for _ in range(3000):
            self.assertEqual(g(t), [2])
        self.assertIs(operator.itemgetter(1)(t), t[1])
        self.assertEqual(operator.itemgetter(-1)(t), "c")
        self.assertEqual(operator.itemgetter(0, 2)(t), (1, "c"))
        with self.assertRaises(IndexError):
            operator.itemgetter(5)(t)
        self.assertEqual(operator.itemgetter(slice(0, 2))(t), (1, [2]))

    def test_overridden_getitem_runs(self):
        g = operator.itemgetter(1)
        for _ in range(3000):
            self.assertEqual(g(Overrides((1, 2))), ("over", 1))

    def test_dict_subclasses(self):
        g = operator.itemgetter("k")
        m = MissingDict(k=1)
        for _ in range(3000):
            self.assertEqual(g(m), 1)
            self.assertEqual(g(MissingDict()), ("missing", "k"))
            self.assertEqual(g(GetitemDict(k=1)), ("own", "k"))
        with self.assertRaises(KeyError):
            g(collections.OrderedDict())

    def test_list_subclass(self):
        class L(list):
            pass
        g = operator.itemgetter(0)
        for _ in range(3000):
            self.assertEqual(g(L(["x"])), "x")
        with self.assertRaises(IndexError):
            g(L())


class Target:
    def __init__(self):
        self.calls = []

    def m(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        return len(self.calls)

    def one(self, a):
        return a * 2


class MethodcallerTests(unittest.TestCase):
    def test_positional_and_keywords(self):
        t = Target()
        self.assertEqual(operator.methodcaller("m")(t), 1)
        self.assertEqual(operator.methodcaller("m", 1, 2)(t), 2)
        self.assertEqual(operator.methodcaller("m", 1, k=3)(t), 3)
        self.assertEqual(t.calls, [((), {}), ((1, 2), {}), ((1,), {"k": 3})])

    def test_hot_loop(self):
        g = operator.methodcaller("one", [7])
        t = Target()
        for _ in range(5000):
            self.assertEqual(g(t), [7, 7])

    def test_instance_attribute_shadows(self):
        t = Target()
        t.one = lambda a: ("own", a)
        self.assertEqual(operator.methodcaller("one", 1)(t), ("own", 1))

    def test_getattribute_override(self):
        class G(Target):
            def __getattribute__(self, name):
                if name == "one":
                    return lambda a: ("ga", a)
                return object.__getattribute__(self, name)
        self.assertEqual(operator.methodcaller("one", 1)(G()), ("ga", 1))

    def test_wrong_arity_and_missing(self):
        with self.assertRaises(TypeError):
            operator.methodcaller("one")(Target())
        with self.assertRaises(AttributeError):
            operator.methodcaller("nope")(Target())

    def test_builtin_receivers(self):
        self.assertEqual(operator.methodcaller("upper")("ab"), "AB")
        self.assertEqual(operator.methodcaller("count", 1)([1, 1, 2]), 2)

    def test_class_method_replaced(self):
        class C:
            def m(self):
                return 1
        g = operator.methodcaller("m")
        c = C()
        for _ in range(3000):
            self.assertEqual(g(c), 1)
        C.m = lambda self: 2
        self.assertEqual(g(c), 2)


class StrSub(str):
    pass


class ReprOnly:
    def __repr__(self):
        return "R!"


class ReprSub:
    def __repr__(self):
        return StrSub("RS")


class BadRepr:
    def __repr__(self):
        return 5


class Both:
    def __str__(self):
        return "S"

    def __repr__(self):
        return "R"


class ReprDunderTests(unittest.TestCase):
    def test_conversions(self):
        for _ in range(3000):
            self.assertEqual(f"{ReprOnly()!r}", "R!")
            self.assertEqual(str(ReprOnly()), "R!")
            self.assertEqual(f"{Both()!r}|{Both()!s}|{Both()}", "R|S|S")
            self.assertEqual(str(Both()), "S")
        self.assertEqual(f"{ReprSub()!r}", "RS")
        self.assertIs(type(repr(ReprSub())), StrSub)
        self.assertIs(type(str(ReprSub())), StrSub)

    def test_non_string_result(self):
        with self.assertRaises(TypeError):
            f"{BadRepr()!r}"
        with self.assertRaises(TypeError):
            repr(BadRepr())
        with self.assertRaises(TypeError):
            str(BadRepr())

    def test_class_changes_seen(self):
        class C:
            def __repr__(self):
                return "one"
        c = C()
        for _ in range(3000):
            self.assertEqual(str(c), "one")
        C.__str__ = lambda self: "str"
        self.assertEqual(str(c), "str")
        del C.__str__
        self.assertEqual(str(c), "one")
        C.__repr__ = lambda self: "two"
        self.assertEqual(f"{c!r}", "two")
        self.assertEqual(str(c), "two")

    def test_base_gains_str(self):
        class Base:
            def __repr__(self):
                return "base"
        class Sub(Base):
            pass
        s = Sub()
        for _ in range(3000):
            self.assertEqual(str(s), "base")
        Base.__str__ = lambda self: "base-str"
        self.assertEqual(str(s), "base-str")


if __name__ == "__main__":
    unittest.main()
