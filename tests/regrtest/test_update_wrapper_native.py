"""functools.wraps/update_wrapper for two plain functions with the default
attribute lists (done natively): names, qualnames, docs, modules,
annotations, type parameters, __dict__ merged with the wrapper's own
entries kept, __wrapped__ set last (even over a copied one), custom
attribute lists and non-function wrappers (the Python body), and a
wrapped function missing an attribute, each matching CPython."""

import functools
import unittest


def wrapped(a: int, b: str = "x") -> bool:
    "The wrapped doc."
    return True


wrapped.extra = 1
wrapped.__wrapped__ = "old"


class Callable:
    def __call__(self):
        return 1


class UpdateWrapperTests(unittest.TestCase):
    def test_wraps(self):
        for i in range(300):
            @functools.wraps(wrapped)
            def w(*args, **kw):
                return wrapped(*args, **kw)
        self.assertEqual(w.__name__, "wrapped")
        self.assertEqual(w.__qualname__, "wrapped")
        self.assertEqual(w.__doc__, "The wrapped doc.")
        self.assertEqual(w.__module__, wrapped.__module__)
        self.assertEqual(w.__annotations__, {"a": int, "b": str, "return": bool})
        self.assertEqual(w.extra, 1)
        self.assertIs(w.__wrapped__, wrapped)
        self.assertTrue(w(1))

    def test_own_dict_kept(self):
        def w():
            pass

        w.mine = 2
        functools.update_wrapper(w, wrapped)
        self.assertEqual((w.mine, w.extra), (2, 1))

    def test_custom_lists_and_callables(self):
        def w():
            pass

        functools.update_wrapper(w, wrapped, assigned=("__doc__",), updated=())
        self.assertEqual(w.__doc__, "The wrapped doc.")
        self.assertEqual(w.__name__, "w")
        self.assertFalse(hasattr(w, "extra"))
        c = Callable()
        functools.update_wrapper(c, wrapped)
        self.assertEqual(c.__name__, "wrapped")
        self.assertIs(c.__wrapped__, wrapped)

    def test_missing_attribute(self):
        def w():
            pass

        p = functools.partial(int, "1")
        functools.update_wrapper(w, p)
        self.assertEqual(w.__name__, "w")
        self.assertIs(w.__wrapped__, p)

    def test_type_params(self):
        def g[T](x: T) -> T:
            return x

        for i in range(300):
            @functools.wraps(g)
            def w(x):
                return x
        self.assertEqual(w.__type_params__, g.__type_params__)


if __name__ == "__main__":
    unittest.main()
