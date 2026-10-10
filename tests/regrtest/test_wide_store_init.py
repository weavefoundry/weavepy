"""Store-only `__init__` bodies with many attributes.

A constructor that only stores its parameters and constants into `self`
is run by its stores alone, up to as many attributes as a class shares
names for. Wide ones (more than sixteen stores), slotted ones and ones
past the limit must all build exactly the instance the body would.
"""

import unittest

NAMES = ["f%d" % i for i in range(34)]


def make_class(n, slots=False):
    params = ", ".join("a%d" % i for i in range(min(n, 7)))
    body = "\n".join(
        "        self.%s = %s" % (NAMES[i], "a%d" % (i % 7) if i % 3 else repr(i))
        for i in range(n)
    )
    src = "class C:\n"
    if slots:
        src += "    __slots__ = %r\n" % (tuple(NAMES[:n]),)
    src += "    def __init__(self, %s):\n%s\n" % (params, body)
    ns = {}
    exec(src, ns)
    return ns["C"], min(n, 7)


def expected(n):
    return {NAMES[i]: (i % 7 if i % 3 else i) for i in range(n)}


class WideInitTest(unittest.TestCase):
    def check(self, n, slots):
        cls, nparams = make_class(n, slots)
        args = list(range(nparams))
        for _ in range(300):
            obj = cls(*args)
        if slots:
            got = {name: getattr(obj, name) for name in NAMES[:n]}
            self.assertFalse(hasattr(obj, "__dict__"))
        else:
            got = vars(obj)
            self.assertEqual(list(got), NAMES[:n])
        self.assertEqual(got, expected(n))

    def test_widths(self):
        for n in (1, 15, 16, 17, 20, 29, 30, 31, 33):
            for slots in (False, True):
                with self.subTest(n=n, slots=slots):
                    self.check(n, slots)

    def test_class_change_between_calls(self):
        cls, nparams = make_class(20)
        for _ in range(300):
            obj = cls(*range(nparams))
        cls.f3 = property(lambda self: "prop", lambda self, v: None)
        obj = cls(*range(nparams))
        self.assertEqual(obj.f3, "prop")
        self.assertNotIn("f3", vars(obj))
        del cls.f3
        obj = cls(*range(nparams))
        self.assertEqual(obj.f3, 3)


if __name__ == "__main__":
    unittest.main()
