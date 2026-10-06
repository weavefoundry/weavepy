"""Exception classes with a Python `__init__` construct on the fast path.

`args` is seeded from the positionals before `__init__` runs, as
`BaseException_new` does, whether or not `__init__` calls
`super().__init__`; `super().__init__(...)` reaches `BaseException.__init__`
and replaces `args`; other native allocators (OSError's errno mapping)
keep their own construction.
"""

import errno
import unittest


class AppError(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code = code


class NotFound(AppError):
    pass


class Naive(Exception):
    def __init__(self, a, b):
        self.total = a + b


class TwoArgs(Exception):
    def __init__(self, a, b):
        super().__init__(a, b)


class MyOSError(OSError):
    def __init__(self, *args):
        super().__init__(*args)
        self.extra = True


class ExceptionConstructionTest(unittest.TestCase):
    def test_super_init_sets_args(self):
        for i in range(500):
            e = NotFound(404, "missing %d" % i)
            self.assertEqual(e.args, ("missing %d" % i,))
            self.assertEqual(e.code, 404)
            self.assertEqual(str(e), "missing %d" % i)
            self.assertIsInstance(e, AppError)

    def test_args_seeded_without_super(self):
        for _ in range(300):
            e = Naive(1, 2)
            self.assertEqual(e.args, (1, 2))
            self.assertEqual(e.total, 3)

    def test_multiple_args(self):
        for _ in range(300):
            e = TwoArgs("x", 2)
            self.assertEqual(e.args, ("x", 2))
            self.assertEqual(repr(e), "TwoArgs('x', 2)")

    def test_raise_and_catch(self):
        caught = 0
        for i in range(300):
            try:
                raise NotFound(404, "m") from None
            except AppError as e:
                caught += e.code == 404 and e.__suppress_context__
        self.assertEqual(caught, 300)

    def test_oserror_subclass(self):
        for _ in range(100):
            e = MyOSError(errno.ENOENT, "nope")
            self.assertEqual(e.errno, errno.ENOENT)
            self.assertEqual(e.strerror, "nope")
            self.assertTrue(e.extra)


if __name__ == "__main__":
    unittest.main()
