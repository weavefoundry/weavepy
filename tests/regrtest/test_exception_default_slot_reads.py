"""Reads of an exception's `args`, `__traceback__` and chaining links.

A never-raised exception's `__traceback__`, `__cause__` and `__context__`
read as `None` and `__suppress_context__` as `False`, in compiled code and
in the interpreter alike; once set they read their values. A subclass's
own property or `__getattribute__`, and an unset `__slots__` member of an
exception subclass, behave as before.
"""

import unittest

N = 3000


class E(Exception):
    pass


class Slotted(Exception):
    __slots__ = ("extra",)


class OwnTraceback(Exception):
    @property
    def __traceback__(self):
        return "own"


class Hooked(Exception):
    def __getattribute__(self, name):
        if name == "__cause__":
            return "hooked"
        return super().__getattribute__(name)


def read_links(e):
    return (e.__traceback__, e.__cause__, e.__context__, e.__suppress_context__, e.args)


class ExceptionSlotReadTests(unittest.TestCase):
    def test_defaults(self):
        e = E(1)
        for _ in range(N):
            r = read_links(e)
        self.assertEqual(r, (None, None, None, False, (1,)))

    def test_set_values(self):
        cause = ValueError("c")
        try:
            try:
                raise KeyError("k")
            except KeyError:
                raise E(2) from cause
        except E as caught:
            e = caught
        for _ in range(N):
            r = read_links(e)
        tb, c, ctx, sup, args = r
        self.assertIsNotNone(tb)
        self.assertIs(tb, e.__traceback__)
        self.assertIs(c, cause)
        self.assertIsInstance(ctx, KeyError)
        self.assertTrue(sup)
        self.assertEqual(args, (2,))

    def test_changing_receivers(self):
        raised = []
        for k in range(4):
            try:
                raise E(k)
            except E as e:
                raised.append(e)
        fresh = [E(k) for k in range(4)]
        out = []
        for i in range(N):
            e = (raised if i % 2 else fresh)[i % 4]
            out.append(e.__traceback__ is None)
        self.assertEqual(out[:4], [True, False, True, False])

    def test_assigned_links(self):
        e = E()
        for i in range(N):
            if i == N // 2:
                e.__cause__ = KeyError()
                e.__traceback__ = None
            r = e.__cause__
        self.assertIsInstance(r, KeyError)
        self.assertTrue(e.__suppress_context__)
        with self.assertRaises(TypeError):
            del e.__traceback__

    def test_overrides(self):
        o, h = OwnTraceback(), Hooked()
        for _ in range(N):
            r = (o.__traceback__, h.__cause__, h.__context__)
        self.assertEqual(r, ("own", "hooked", None))

    def test_unset_slots_member(self):
        s = Slotted()
        for _ in range(500):
            with self.assertRaises(AttributeError):
                s.extra
            self.assertIsNone(s.__traceback__)
        s.extra = 5
        self.assertEqual(s.extra, 5)


if __name__ == "__main__":
    unittest.main()
