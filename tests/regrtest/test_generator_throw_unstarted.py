"""`throw()` into a generator that never started.

A `StopIteration` thrown into a generator before its first step escapes
unconverted (PEP 479's conversion covers the body past its start) and
finishes the generator; one thrown into a started generator becomes a
`RuntimeError`, and other exceptions behave as before.
"""

import unittest


def gen():
    yield 1
    yield 2


class ThrowUnstartedTests(unittest.TestCase):
    def test_unstarted_stop_iteration(self):
        for g in (gen(), (x for x in [1, 2])):
            with self.assertRaises(StopIteration) as cm:
                g.throw(StopIteration(5))
            self.assertEqual(cm.exception.args, (5,))
            self.assertEqual(list(g), [])

    def test_started_stop_iteration(self):
        g = gen()
        next(g)
        with self.assertRaises(RuntimeError) as cm:
            g.throw(StopIteration)
        self.assertIsInstance(cm.exception.__cause__, StopIteration)

    def test_other_exceptions(self):
        g = gen()
        with self.assertRaises(KeyError):
            g.throw(KeyError("k"))
        self.assertEqual(list(g), [])
        g = gen()
        g.close()
        self.assertEqual(list(g), [])


if __name__ == "__main__":
    unittest.main()
