"""Attribute reads off functions, and stores to a plain class's `args`.

A function's stored attributes (`__name__`, `__doc__`, `__wrapped__`,
values set on it) are read in place, through `f.name` and through
`getattr`. Names the function resolves through its class first, or
computes, must still come from the full lookup, and changes must show at
once. A plain class's `args` attribute is an ordinary attribute; only an
exception's goes through `BaseException`'s coercion.
"""

import functools
import unittest


def plain():
    """Plain's docstring."""
    return 1


def undocumented():
    return 2


class FunctionAttrReadTest(unittest.TestCase):
    def test_slots_and_dict(self):
        @functools.wraps(plain)
        def wrapper():
            return plain()

        for _ in range(2000):
            self.assertEqual(plain.__doc__, "Plain's docstring.")
            self.assertIsNone(undocumented.__doc__)
            self.assertEqual(plain.__name__, "plain")
            self.assertIs(wrapper.__wrapped__, plain)
            self.assertEqual(getattr(plain, "__doc__", None), "Plain's docstring.")
            self.assertIs(getattr(wrapper, "__wrapped__"), plain)
            self.assertEqual(getattr(plain, "missing", 5), 5)

    def test_changes_show(self):
        def f():
            pass

        seen = []
        for i in range(2000):
            if i == 1000:
                f.__doc__ = "changed"
                f.tag = "t"
            seen.append((f.__doc__, getattr(f, "tag", None)))
        self.assertEqual(seen[999], (None, None))
        self.assertEqual(seen[1000], ("changed", "t"))
        del f.tag
        self.assertIsNone(getattr(f, "tag", None))
        with self.assertRaises(AttributeError):
            f.tag

    def test_class_names_resolve_first(self):
        def f():
            pass

        f.__dict__["__class__"] = 1
        for _ in range(500):
            self.assertIs(f.__class__, type(plain))
            self.assertIs(getattr(f, "__class__"), type(plain))

    def test_plain_args_attribute(self):
        class Holder:
            def __init__(self, args):
                self.args = args

        for i in range(2000):
            h = Holder([i])
            self.assertEqual(h.args, [i])

        class Err(Exception):
            def __init__(self, args):
                self.args = args

        for i in range(2000):
            e = Err([i])
            self.assertEqual(e.args, (i,))


if __name__ == "__main__":
    unittest.main()
