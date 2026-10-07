"""`next(gen)` of a simple generator stepped in place.

The core loop runs a simple generator body to its next yield without an
activation of its own. The yielded value, the return value a finishing
body raises in `StopIteration`, a generator that is already finished, an
error from the body, and the finalizer of a generator abandoned mid-body
must all match the ordinary resume.
"""

import gc
import unittest


def three():
    yield 1
    yield 2
    return "done"


class NextStepsInPlaceTest(unittest.TestCase):
    def test_values_and_return(self):
        for _ in range(2000):
            g = three()
            self.assertEqual(next(g), 1)
            self.assertEqual(next(g), 2)
            with self.assertRaises(StopIteration) as cm:
                next(g)
            self.assertEqual(cm.exception.value, "done")
            with self.assertRaises(StopIteration) as cm:
                next(g)
            self.assertIsNone(cm.exception.value)

    def test_abandoned_generator_closes(self):
        log = []

        def closing():
            try:
                yield 1
                yield 2
            finally:
                log.append("closed")

        for _ in range(200):
            self.assertEqual(next(closing()), 1)
        gc.collect()
        self.assertEqual(log.count("closed"), 200)

    def test_body_error(self):
        def bad():
            yield 1
            raise ValueError("x")

        for _ in range(500):
            g = bad()
            next(g)
            with self.assertRaises(ValueError):
                next(g)

    def test_running_generator_refuses(self):
        def selfish():
            yield next(me)

        me = selfish()
        with self.assertRaises(ValueError):
            next(me)


if __name__ == "__main__":
    unittest.main()
