"""Python iterator classes stepped frameless, and their ends.

`for` loops, `list()` and `next()` step a plain iterator class's
`__next__` without a frame, and a bare `raise StopIteration` ends the
loop without the exception being built. Stores made before the raise,
other exceptions, `StopIteration` subclasses and values, finalizers, tracing,
and `next()` with and without a default behave as before.
"""

import gc
import sys
import unittest

N = 3000


class Bounded:
    def __init__(self, n):
        self.i = 0
        self.n = n

    def __iter__(self):
        return self

    def __next__(self):
        i = self.i
        if i >= self.n:
            raise StopIteration
        self.i = i + 1
        return i


class Counter:
    def __init__(self):
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        self.i += 1
        return self.i


class MarksDone:
    def __init__(self, n):
        self.i = 0
        self.n = n
        self.done = False

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            self.done = True
            raise StopIteration
        self.i += 1
        return self.i


class MyStop(StopIteration):
    pass


class SubclassEnd(Bounded):
    def __next__(self):
        if self.i >= self.n:
            raise MyStop
        self.i += 1
        return self.i


class RaisesKey(Bounded):
    def __next__(self):
        if self.i >= self.n:
            raise KeyError(self.i)
        self.i += 1
        return self.i


class PythonIteratorEndTests(unittest.TestCase):
    def test_for_and_list(self):
        total = 0
        for _ in range(N):
            for x in Bounded(5):
                total += x
        self.assertEqual(total, 10 * N)
        for _ in range(N):
            r = list(Bounded(4))
        self.assertEqual(r, [0, 1, 2, 3])
        b = Bounded(3)
        for _ in range(N):
            for x in b:
                pass
        self.assertEqual(b.i, 3)

    def test_next(self):
        c = Counter()
        for _ in range(N):
            v = next(c)
        self.assertEqual(v, N)
        b = Bounded(0)
        for _ in range(N):
            self.assertIsNone(next(b, None))
        for _ in range(N):
            try:
                next(b)
            except StopIteration as e:
                caught = e
        self.assertIsNone(caught.value)
        names = []
        tb = caught.__traceback__
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names, ["test_next", "__next__"])

    def test_store_before_raise(self):
        for _ in range(N):
            m = MarksDone(2)
            self.assertEqual(list(m), [1, 2])
            self.assertTrue(m.done)
        for _ in range(N):
            m = MarksDone(1)
            for _ in m:
                pass
            self.assertTrue(m.done)

    def test_other_ends(self):
        for _ in range(N):
            self.assertEqual(list(SubclassEnd(2)), [1, 2])
        for _ in range(N):
            got = []
            with self.assertRaises(KeyError):
                for x in RaisesKey(2):
                    got.append(x)
        self.assertEqual(got, [1, 2])

    def test_value_and_generator(self):
        class WithValue(Bounded):
            def __next__(self):
                if self.i >= self.n:
                    raise StopIteration(7)
                self.i += 1
                return self.i

        for _ in range(N):
            try:
                next(WithValue(0))
            except StopIteration as e:
                self.assertEqual(e.value, 7)

        def gen():
            yield next(Bounded(0))

        for _ in range(300):
            with self.assertRaises(RuntimeError) as cm:
                list(gen())
            self.assertIsInstance(cm.exception.__cause__, StopIteration)

    def test_finalizer_at_end(self):
        deleted = []

        class Fin(Bounded):
            def __del__(self):
                deleted.append(self.n)

        for k in range(N):
            for x in Fin(2):
                pass
        gc.collect()
        self.assertEqual(len(deleted), N)

    def test_tracing_sees_raise(self):
        b = Bounded(1)
        for _ in range(N):
            b.i = 0
            list(b)
        events = []

        def tracer(frame, event, arg):
            if event == "exception" and frame.f_code.co_name == "__next__":
                events.append(arg[0])
            return tracer

        sys.settrace(tracer)
        try:
            b.i = 0
            r = list(b)
        finally:
            sys.settrace(None)
        self.assertEqual(r, [0])
        self.assertEqual(events, [StopIteration])


if __name__ == "__main__":
    unittest.main()
