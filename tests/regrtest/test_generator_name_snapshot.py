"""A generator snapshots its function's `__name__` and `__qualname__` at
creation. A function whose getset slots were read (as `functools.wraps`
reads them) remembers the slots' state in which it last found its code's
own names there, so later generators skip the comparison (see
`PyFunction::code_names_kept` in the VM). Each case makes generators warm,
then renames or swaps the code, and checks what new generators report.
"""

import functools
import unittest

WARM = 2000


def plain():
    yield 1


def other():
    yield 2


class GeneratorNameSnapshotTests(unittest.TestCase):
    def make(self):
        def gen(x):
            yield x

        # Reading the slots the way `wraps` does materializes them.
        @functools.wraps(gen)
        def wrapper(*a):
            return gen(*a)

        return gen, wrapper

    def test_wrapped_names_warm(self):
        gen, wrapper = self.make()
        for i in range(WARM):
            g = wrapper(i)
        self.assertEqual(g.__name__, "gen")
        self.assertTrue(g.__qualname__.endswith("make.<locals>.gen"))
        self.assertIs(g.__name__, gen.__name__)
        self.assertIs(g.__qualname__, gen.__qualname__)

    def test_rename_after_warm(self):
        gen, wrapper = self.make()
        for i in range(WARM):
            wrapper(i)
        gen.__name__ = "renamed"
        g = wrapper(0)
        self.assertEqual(g.__name__, "renamed")
        self.assertIs(g.__name__, gen.__name__)
        gen.__qualname__ = "Q.renamed"
        g = wrapper(0)
        self.assertEqual(g.__qualname__, "Q.renamed")
        for i in range(WARM):
            g = wrapper(i)
        self.assertEqual((g.__name__, g.__qualname__), ("renamed", "Q.renamed"))

    def test_rename_back_to_equal_text(self):
        gen, wrapper = self.make()
        for i in range(WARM):
            wrapper(i)
        text = "".join(["g", "en"])
        gen.__name__ = text
        g = wrapper(0)
        self.assertEqual(g.__name__, "gen")
        self.assertIs(g.__name__, gen.__name__)

    def test_code_swap(self):
        def f():
            yield 1

        functools.update_wrapper(lambda: None, f)
        for _ in range(WARM):
            g = f()
        self.assertEqual(g.__name__, "f")
        f.__code__ = other.__code__
        g = f()
        self.assertEqual(next(g), 2)
        self.assertEqual(g.__name__, f.__name__)
        self.assertEqual(g.__qualname__, f.__qualname__)

    def test_unread_slots(self):
        for _ in range(WARM):
            g = plain()
        self.assertEqual(g.__name__, "plain")
        self.assertEqual(g.__qualname__, "plain")

    def test_generator_name_reassigned(self):
        gen, wrapper = self.make()
        for i in range(WARM):
            g = wrapper(i)
        g.__name__ = "mine"
        self.assertEqual(g.__name__, "mine")
        self.assertEqual(wrapper(1).__name__, "gen")


if __name__ == "__main__":
    unittest.main()
