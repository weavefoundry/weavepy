"""Dropping a suspended generator closes it as CPython's gen_close does.

A generator suspended outside any handler of its own finishes at once
when its last reference goes, running nothing. One suspended inside a
`try` gets `GeneratorExit` (its `finally` and `except` run), and one
suspended in `yield from` closes its delegate first.
"""

import gc
import unittest
import weakref


class GeneratorReleaseCloseTest(unittest.TestCase):
    def test_plain_suspension_finishes_quietly(self):
        log = []

        def gen():
            log.append("start")
            yield 1
            log.append("after")

        for _ in range(500):
            g = gen()
            next(g)
            del g
        self.assertEqual(log, ["start"] * 500)

    def test_finally_runs(self):
        log = []

        def gen():
            try:
                yield 1
            finally:
                log.append("finally")

        for _ in range(300):
            g = gen()
            next(g)
            del g
        self.assertEqual(log, ["finally"] * 300)

    def test_generator_exit_is_seen(self):
        seen = []

        def gen():
            try:
                yield 1
            except GeneratorExit:
                seen.append(True)
                raise

        g = gen()
        next(g)
        del g
        self.assertEqual(seen, [True])

    def test_delegate_closed(self):
        log = []

        def inner():
            try:
                yield 1
            finally:
                log.append("inner")

        def outer():
            yield from inner()

        g = outer()
        next(g)
        del g
        self.assertEqual(log, ["inner"])

    def test_locals_released_and_weakref_cleared(self):
        class Box:
            pass

        def gen(b):
            yield b

        b = Box()
        ref = weakref.ref(b)
        g = gen(b)
        next(g)
        gref = weakref.ref(g)
        del b, g
        self.assertIsNone(gref())
        self.assertIsNone(ref())

    def test_frame_inspected(self):
        def gen():
            x = 42
            yield x

        g = gen()
        next(g)
        frame = g.gi_frame
        self.assertEqual(frame.f_locals["x"], 42)
        del g
        gc.collect()

    def test_self_cycle(self):
        def gen():
            me = yield
            yield me

        g = gen()
        next(g)
        g.send(g)
        ref = weakref.ref(g)
        del g
        gc.collect()
        self.assertIsNone(ref())


if __name__ == "__main__":
    unittest.main()
