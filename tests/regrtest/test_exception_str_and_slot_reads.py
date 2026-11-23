"""`str(e)`, `f"{e}"` and pseudo-slot reads of exceptions in compiled loops.

`BaseException.__str__` is called with the instance (no bound method), in
place for `str(e)` when the argument runs no code of its own; an exception
site's `e.args`, `e.__context__` and kin read the instance's laid-out
slots directly once the site knows the class. Each must agree with the
full paths: every argument shape, `KeyError`'s repr, a subclass with its
own `__str__` (added after the reads warmed up, too), a nested exception
argument, an argument whose `__str__` raises, unset and set chaining
slots, a class that shadows `args` with a property, and `__slots__` on an
exception subclass.
"""

import unittest


class E(Exception):
    pass


class Loud:
    def __str__(self):
        return "loud"


class Bad:
    def __str__(self):
        raise RuntimeError("no str")


class WithArgsProp(Exception):
    @property
    def args(self):
        return ("prop",)


class Slotted(Exception):
    __slots__ = ("extra",)


def str_many(e, n=200):
    out = None
    for _ in range(n):
        out = str(e)
    return out


def fmt_many(e, n=200):
    out = None
    for _ in range(n):
        out = f"{e}"
    return out


class ExceptionStrTests(unittest.TestCase):
    def test_shapes(self):
        cases = [
            E(), E("x"), E(1), E(1, 2), E(None), E(1.5), E((1, 2)),
            E("\udcac"), KeyError("k"), KeyError(3), KeyError(), ValueError("v"),
            E(E("inner")), E(Loud()), KeyError(Loud()), OSError(2, "no"),
            E([1, "a"]), E(b"b"),
        ]
        wants = [
            "", "x", "1", "(1, 2)", "None", "1.5", "(1, 2)", "\udcac", "'k'", "3",
            "", "v", "inner", "loud", None, "[Errno 2] no", "[1, 'a']", "b'b'",
        ]
        for e, want in zip(cases, wants):
            with self.subTest(e=ascii(e)):
                if want is None:
                    # (`KeyError`'s repr of an instance argument.)
                    want = repr(e.args[0])
                self.assertEqual(str_many(e), want)
                self.assertEqual(fmt_many(e), want)

    def test_argument_str_raises(self):
        e = E(Bad())
        for _ in range(100):
            with self.assertRaises(RuntimeError):
                str(e)
            with self.assertRaises(RuntimeError):
                f"{e}"

    def test_arguments_with_python_repr(self):
        calls = []

        class R:
            def __repr__(self):
                calls.append(1)
                return "R!"

        e1, e2 = E([R()]), E(R(), 2)
        for _ in range(100):
            self.assertEqual(str(e1), "[R!]")
            self.assertEqual(f"{e1}", "[R!]")
            self.assertEqual(str(e2), "(R!, 2)")
            self.assertEqual(f"{e2}", "(R!, 2)")
        self.assertEqual(len(calls), 400)

    def test_override_after_warm_up(self):
        class F(Exception):
            pass

        e = F("a")
        self.assertEqual(str_many(e), "a")
        F.__str__ = lambda self: "over"
        self.assertEqual(str_many(e), "over")
        self.assertEqual(fmt_many(e), "over")
        del F.__str__
        self.assertEqual(str_many(e), "a")

    def test_str_returns_non_string(self):
        class G(Exception):
            pass

        G.__str__ = Exception.__repr__
        self.assertEqual(str_many(G(1)), "G(1)")


class ExceptionSlotReadTests(unittest.TestCase):
    def test_args_and_chaining(self):
        e = E(1, 2)
        for _ in range(300):
            self.assertEqual(e.args, (1, 2))
            self.assertIsNone(e.__context__)
            self.assertIsNone(e.__cause__)
            self.assertIs(e.__suppress_context__, False)
            self.assertIsNone(e.__traceback__)
        try:
            try:
                raise KeyError("a")
            except KeyError as inner:
                raise E("b") from inner
        except E as caught:
            outer = caught
        for _ in range(300):
            self.assertIsInstance(outer.__cause__, KeyError)
            self.assertIs(outer.__context__, outer.__cause__)
            self.assertIs(outer.__suppress_context__, True)
            self.assertIsNotNone(outer.__traceback__)
            self.assertEqual(outer.args, ("b",))
        outer.args = ("c", 3)
        outer.__context__ = None
        for _ in range(300):
            self.assertEqual(outer.args, ("c", 3))
            self.assertIsNone(outer.__context__)

    def test_mixed_classes_at_one_site(self):
        items = [E(1), KeyError(2), WithArgsProp(3), OSError(2, "x"), Slotted(5)]
        for _ in range(200):
            got = [x.args for x in items]
            self.assertEqual(got, [(1,), (2,), ("prop",), (2, "x"), (5,)])

    def test_slotted_subclass(self):
        s = Slotted("m")
        s.extra = 7
        for _ in range(300):
            self.assertEqual(s.args, ("m",))
            self.assertEqual(s.extra, 7)
            self.assertIsNone(s.__context__)


if __name__ == "__main__":
    unittest.main()
