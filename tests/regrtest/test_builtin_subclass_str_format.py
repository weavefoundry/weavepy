"""str(), format() and f-strings of str, int and float subclass instances
(IntEnum, StrEnum, IntFlag, plain subclasses) from compiled loops: the
payload's text where the class keeps its base's methods, a spec
formatted against the payload, and every override (`__str__`,
`__repr__`, `__format__`, one added after the loop warmed up) still
taking effect, each matching CPython."""

import enum
import unittest


class I(int):
    pass


class F(float):
    pass


class S(str):
    pass


class Num(enum.IntEnum):
    A = 1
    B = 22


class St(enum.StrEnum):
    A = "a"
    B = "bee"


class Color(enum.Enum):
    RED = 1


class Perm(enum.IntFlag):
    R = 4
    W = 2


class OwnRepr(int):
    def __repr__(self):
        return "OwnRepr!"


class OwnStr(int):
    def __str__(self):
        return "OwnStr!"


class OwnFormat(int):
    def __format__(self, spec):
        return "fmt:" + spec


class Late(int):
    pass


def render(n, x):
    out = None
    for i in range(n):
        out = (str(x), f"{x}", format(x), "%s" % (x,), "{}".format(x))
    return out


def spec(n, x, s):
    out = None
    for i in range(n):
        out = (f"{x:>6}", format(x, s))
    return out


class SubclassStrFormatTests(unittest.TestCase):
    def test_payload_text(self):
        cases = [
            (I(5), "5"),
            (I(-12), "-12"),
            (F(2.5), "2.5"),
            (S("x"), "x"),
            (Num.B, "22"),
            (St.B, "bee"),
            (I(10**30), str(10**30)),
        ]
        for x, want in cases:
            for n in (1, 400):
                got = render(n, x)
                self.assertEqual(got, (want,) * 5, x)
                self.assertTrue(all(type(g) is str for g in got))

    def test_specs(self):
        for n in (1, 400):
            self.assertEqual(spec(n, Num.B, "x"), ("    22", "16"))
            self.assertEqual(spec(n, I(255), "08b"), ("   255", "11111111"))
            self.assertEqual(spec(n, F(1.25), ".1f"), ("  1.25", "1.2"))
            self.assertEqual(spec(n, St.B, "^7"), ("   bee", "  bee  "))
            self.assertEqual(spec(n, S("q"), "<3"), ("     q", "q  "))
        with self.assertRaises(ValueError):
            spec(300, St.A, "d")

    def test_overrides(self):
        for n in (1, 400):
            self.assertEqual(render(n, OwnRepr(3)), ("OwnRepr!",) * 5)
            self.assertEqual(render(n, OwnStr(3)), ("OwnStr!",) * 5)
            self.assertEqual(render(n, OwnFormat(3))[1:3], ("fmt:", "fmt:"))
            self.assertEqual(render(n, OwnFormat(3))[0], "3")
            self.assertEqual(render(n, Color.RED), ("Color.RED",) * 5)
            self.assertEqual(render(n, Perm.R | Perm.W), ("6",) * 5)

    def test_override_added_later(self):
        self.assertEqual(render(400, Late(7)), ("7",) * 5)
        Late.__str__ = lambda self: "late"
        try:
            self.assertEqual(render(400, Late(7)), ("late",) * 5)
        finally:
            del Late.__str__
        Late.__format__ = lambda self, s: "F" + s
        try:
            self.assertEqual(render(400, Late(7))[1:3], ("F", "F"))
        finally:
            del Late.__format__
        self.assertEqual(render(400, Late(7)), ("7",) * 5)

    def test_join_and_percent(self):
        class SS(str):
            def __str__(self):
                return "zzz"

        def j(n, items):
            out = None
            for i in range(n):
                out = "-".join(items)
            return out

        def pct(n, fmt, args):
            out = None
            for i in range(n):
                out = fmt % args
            return out

        for n in (1, 400):
            self.assertEqual(j(n, (St.A, St.B)), "a-bee")
            self.assertEqual(j(n, [S("x"), "y", St.A]), "x-y-a")
            self.assertEqual(j(n, [SS("a"), SS("b")]), "a-b")
            solo = j(n, [S("solo")])
            self.assertEqual(solo, "solo")
            self.assertIs(type(solo), str)
            self.assertEqual(pct(n, "%s", St.B), "bee")
            self.assertEqual(pct(n, "%s/%s", (Num.B, I(3))), "22/3")
            self.assertEqual(pct(n, "%s %%", (F(1.5),)), "1.5 %")
            self.assertEqual(pct(n, "%r", (St.B,)), "<St.B: 'bee'>")
            self.assertEqual(pct(n, "%d", (Num.B,)), "22")
            self.assertEqual(pct(n, "%s", (Color.RED,)), "Color.RED")
            self.assertEqual(pct(n, "%s", (OwnStr(1),)), "OwnStr!")
            self.assertEqual(pct(n, "%s", (SS("q"),)), "zzz")
        with self.assertRaises(TypeError):
            j(300, [St.A, 1])
        with self.assertRaises(TypeError):
            pct(300, "%s %s", (St.A,))

    def test_plain_enums(self):
        class Shade(enum.Enum):
            DARK = 1
            LIGHT = 2

        class Mixed(int, enum.Enum):
            ONE = 1

        class Custom(enum.Enum):
            X = 1

            def __str__(self):
                return "custom"

        class Bits(enum.Flag):
            A = 1
            B = 2

        for n in (1, 400):
            self.assertEqual(render(n, Shade.DARK), ("Shade.DARK",) * 5)
            self.assertEqual(spec(n, Shade.LIGHT, "<12"), ("Shade.LIGHT", "Shade.LIGHT "))
            self.assertEqual(render(n, Mixed.ONE), ("Mixed.ONE",) * 5)
            self.assertEqual(render(n, Custom.X), ("custom",) * 5)
            self.assertEqual(render(n, Bits.A | Bits.B), ("Bits.A|B",) * 5)
            self.assertEqual(render(n, Bits.A), ("Bits.A",) * 5)
        Shade.__name__ = "Tone"
        try:
            self.assertEqual(render(400, Shade.DARK), ("Tone.DARK",) * 5)
        finally:
            Shade.__name__ = "Shade"
        Shade.__str__ = lambda self: "late"
        try:
            self.assertEqual(render(400, Shade.DARK), ("late",) * 5)
        finally:
            del Shade.__str__
        self.assertEqual(render(400, Shade.DARK), ("Shade.DARK",) * 5)

    def test_profiler_sees_enum_methods(self):
        import sys

        render(400, Color.RED)
        seen = []

        def prof(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)

        sys.setprofile(prof)
        try:
            render(1, Color.RED)
        finally:
            sys.setprofile(None)
        self.assertIn("__str__", seen)
        self.assertIn("__format__", seen)

    def test_percent_temporaries(self):
        dead = []

        class DS(str):
            def __del__(self):
                dead.append(1)

        def f(n):
            out = None
            for i in range(n):
                out = "%s" % DS("t")
                out = "%s|%s" % (DS("u"), 1)
            return out

        self.assertEqual(f(300), "u|1")
        self.assertEqual(len(dead), 600)

    def test_temporaries(self):
        def temp(n):
            out = []
            for i in range(n):
                out.append(f"{I(i)}")
            return out

        self.assertEqual(temp(300), [str(i) for i in range(300)])


if __name__ == "__main__":
    unittest.main()
