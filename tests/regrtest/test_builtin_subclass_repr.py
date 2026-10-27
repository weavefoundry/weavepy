"""`repr` and `str` of built-in subclass instances, alone and in containers.

A subclass of a built-in value or container type that doesn't override
`__repr__` renders as its base does wherever it appears: inside a tuple,
list, dict, set, deque or `OrderedDict`, through `%r` and `{!r}`, and
nested. One that does override `__repr__` renders through the override,
and so does its `str()` when the base has no `__str__` of its own (`int`,
`float`, `complex`, `list`, `dict`, `tuple`): `print`, f-strings, `%s`
and `format` included.
"""

import collections
import unittest


class L(list):
    pass


class D(dict):
    pass


class T(tuple):
    pass


class S(str):
    pass


class I(int):
    pass


class F(float):
    pass


class C(complex):
    pass


class B(bytes):
    pass


class St(set):
    pass


class BA(bytearray):
    pass


class LR(list):
    def __repr__(self):
        return "LR!"


class DR(dict):
    def __repr__(self):
        return "DR!"


class TR(tuple):
    def __repr__(self):
        return "TR!"


class IR(int):
    def __repr__(self):
        return "IR!"


class FR(float):
    def __repr__(self):
        return "FR!"


class CR(complex):
    def __repr__(self):
        return "CR!"


class SR(str):
    def __repr__(self):
        return "SR!"


class ContainerReprTests(unittest.TestCase):
    def check(self, value, text):
        for _ in range(3):
            self.assertEqual(repr(value), text)
            self.assertEqual(repr((None, value)), f"(None, {text})")
            self.assertEqual(repr([value]), f"[{text}]")
            self.assertEqual(repr({1: value}), f"{{1: {text}}}")
            self.assertEqual(str([value]), f"[{text}]")
            self.assertEqual(f"{(value,)}", f"({text},)")
            self.assertEqual("%r" % (value,), text)
            self.assertEqual("%s" % ((value,),), f"({text},)")
            self.assertEqual(f"{value!r}", text)
            self.assertEqual(repr([[value]]), f"[[{text}]]")
            self.assertEqual(repr(collections.deque([value])), f"deque([{text}])")
            self.assertEqual(
                repr(collections.OrderedDict(a=value)), f"OrderedDict({{'a': {text}}})"
            )

    def test_without_repr(self):
        self.check(L([9]), "[9]")
        self.check(D(a=1), "{'a': 1}")
        self.check(T((1, 2)), "(1, 2)")
        self.check(S("s"), "'s'")
        self.check(I(5), "5")
        self.check(F(1.5), "1.5")
        self.check(C(1j), "1j")
        self.check(B(b"x"), "b'x'")
        self.check(St({3}), "St({3})")
        self.check(BA(b"x"), "BA(b'x')")

    def test_with_repr(self):
        for value, text in [
            (LR([1]), "LR!"),
            (DR(a=1), "DR!"),
            (TR((1,)), "TR!"),
            (IR(2), "IR!"),
            (FR(1.5), "FR!"),
            (CR(1j), "CR!"),
            (SR("s"), "SR!"),
        ]:
            self.check(value, text)

    def test_hashable_in_set(self):
        self.assertEqual(repr({T((1,))}), "{(1,)}")
        self.assertEqual(repr({IR(2)}), "{IR!}")

    def test_recursive(self):
        lst = L()
        lst.append(lst)
        self.assertEqual(repr(lst), "[[...]]")
        self.assertEqual(repr([lst]), "[[[...]]]")
        d = D()
        d["k"] = d
        self.assertEqual(repr((d,)), "({'k': {...}},)")


class StrTests(unittest.TestCase):
    def check_str(self, value, text):
        for _ in range(3):
            self.assertEqual(str(value), text)
            self.assertEqual(f"{value}", text)
            self.assertEqual("%s" % (value,), text)
            self.assertEqual(format(value), text)
            self.assertEqual("{}".format(value), text)

    def test_override_used_without_tp_str(self):
        self.check_str(IR(2), "IR!")
        self.check_str(FR(1.5), "FR!")
        self.check_str(CR(1j), "CR!")
        self.check_str(LR([1]), "LR!")
        self.check_str(DR(a=1), "DR!")
        self.check_str(TR((1,)), "TR!")

    def test_str_keeps_its_own(self):
        self.check_str(SR("s"), "s")

    def test_without_override(self):
        self.check_str(I(5), "5")
        self.check_str(F(1.5), "1.5")
        self.check_str(L([9]), "[9]")
        self.check_str(D(a=1), "{'a': 1}")


if __name__ == "__main__":
    unittest.main()
