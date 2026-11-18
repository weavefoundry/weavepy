"""`for x in it` over an iterator class whose `__next__` isn't a leaf.

The loop calls `__next__` with its activation waiting (no full step);
a `StopIteration` from it, however deep, ends the loop as `FOR_ITER`
ends it, other exceptions propagate with their tracebacks, and the
iterator's own state is what Python code sees.
"""

import sys
import unittest

N = 3000


class Lines:
    def __init__(self, data):
        self.data = list(data)
        self.i = 0

    def __iter__(self):
        return self

    def _read(self):
        if self.i >= len(self.data):
            raise StopIteration
        v = self.data[self.i]
        self.i += 1
        return v

    def __next__(self):
        return self._read().upper()


class Deep(Lines):
    def __next__(self):
        return next(iter(self._read()))


class Boom(Lines):
    def __next__(self):
        if self.i == 2:
            raise ValueError("boom")
        return self._read()


class Framey(Lines):
    def __next__(self):
        v = self._read()
        return (v, sys._getframe(1).f_code.co_name)


def collect(it):
    out = []
    for x in it:
        out.append(x)
    return out


class PythonIteratorLaneTests(unittest.TestCase):
    def test_loops(self):
        for _ in range(N):
            a = collect(Lines("abc"))
            b = collect(Deep(["xy", "z"]))
        self.assertEqual(a, ["A", "B", "C"])
        self.assertEqual(b, ["x", "z"])

    def test_errors(self):
        for _ in range(100):
            try:
                collect(Boom("abcd"))
            except ValueError as e:
                tb = e.__traceback__
            else:
                self.fail("no ValueError")
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-2:], ["collect", "__next__"])

    def test_frames_and_resume(self):
        for _ in range(N):
            r = collect(Framey("ab"))
        self.assertEqual(r, [("a", "collect"), ("b", "collect")])
        it = Lines("abcd")
        for x in it:
            if x == "B":
                break
        self.assertEqual(collect(it), ["C", "D"])


if __name__ == "__main__":
    unittest.main()
