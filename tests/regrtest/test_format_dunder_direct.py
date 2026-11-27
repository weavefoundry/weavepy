"""Compiled f-string formatting calls a class's Python `__format__`
directly; the text with and without a spec, a `str` subclass result, a
non-string result's TypeError, raises (caught in the frame or past it),
objects without their own `__format__`, and a method replaced later
match the general formatting."""

import unittest


class K:
    def __init__(self, a, b):
        self.a = a
        self.b = b
        self.calls = 0

    def fmt(self):
        return "K(%r, %r)" % (self.a, self.b)

    def __format__(self, spec):
        self.calls += 1
        return format(self.fmt(), spec)


class S(str):
    pass


class SubResult:
    def __format__(self, spec):
        return S("sub" + spec)


class Bad:
    def __format__(self, spec):
        return 42


class Raiser:
    def __format__(self, spec):
        raise KeyError(spec)


class Plain:
    def __repr__(self):
        return "Plain()"


class FormatDunderDirect(unittest.TestCase):
    def test_text(self):
        k = K(1, "x")
        for i in range(3000):
            self.assertEqual(f"{k}", "K(1, 'x')")
            self.assertEqual(f"<{k:>12}>", "<   K(1, 'x')>")
            self.assertEqual(f"{k:.3}", "K(1")
        self.assertEqual(k.calls, 9000)

    def test_dynamic_spec(self):
        k = K(2, 3)
        for w in range(500):
            width = w % 15
            self.assertEqual(f"{k:{width}}", format(k.fmt(), str(width)))

    def test_subclass_result(self):
        o = SubResult()
        for i in range(1000):
            r = f"{o:z}"
            self.assertEqual(r, "subz")
            self.assertEqual(f"[{o}]", "[sub]")

    def test_bad_result(self):
        b = Bad()
        for i in range(500):
            with self.assertRaises(TypeError) as cm:
                f"{b}"
            self.assertEqual(str(cm.exception), "__format__ must return a str, not int")

    def test_raises(self):
        r = Raiser()
        n = 0
        for i in range(1000):
            try:
                f"{r:q}"
            except KeyError as e:
                n += 1
                self.assertEqual(e.args, ("q",))
        self.assertEqual(n, 1000)

        def show(o):
            return f"({o})"

        for i in range(1000):
            with self.assertRaises(KeyError):
                show(r)
            self.assertEqual(show(K(i, 0)), "(K(%d, 0))" % i)

    def test_plain_objects(self):
        p = Plain()
        for i in range(1000):
            self.assertEqual(f"{p!r}", "Plain()")
            self.assertEqual(f"{p}", "Plain()")
            self.assertEqual(f"{i:03}", "%03d" % i)

    def test_replaced(self):
        class C:
            def __format__(self, spec):
                return "a"

        c = C()
        out = []
        for i in range(1000):
            out.append(f"{c}")
        C.__format__ = lambda self, spec: "b" + spec
        for i in range(1000):
            out.append(f"{c:s}")
        del C.__format__
        self.assertTrue(f"{c}".startswith("<"))
        self.assertEqual(out.count("a"), 1000)
        self.assertEqual(out.count("bs"), 1000)


if __name__ == "__main__":
    unittest.main()
