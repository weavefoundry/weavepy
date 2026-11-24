"""Native classmethods (chain.from_iterable, dict.fromkeys, bytes.fromhex,
float.fromhex, int.from_bytes) called through their class or a subclass
from compiled loops: each call gives the general call's value, a subclass
gets an instance of itself, Python code the call runs (an iterable's
__iter__, a subclass's __setitem__) runs at the call, and errors raise at
the call."""

import itertools
import unittest


class Chain(itertools.chain):
    pass


class D(dict):
    def __setitem__(self, k, v):
        super().__setitem__(k, v * 2 if isinstance(v, int) else v)


class It:
    def __init__(self):
        self.seen = 0

    def __iter__(self):
        self.seen += 1
        return iter([[self.seen], [0]])


class Bad:
    def __iter__(self):
        raise RuntimeError("no")


def chains(n, it):
    out = None
    xs = [[1], [2, 3]]
    for i in range(n):
        out = (
            list(itertools.chain.from_iterable(xs)),
            list(itertools.chain.from_iterable(it)),
            type(Chain.from_iterable(xs)).__name__,
            list(Chain.from_iterable(x for x in xs)),
        )
    return out


def others(n):
    out = None
    for i in range(n):
        out = (
            dict.fromkeys("ab", 0),
            D.fromkeys("ab", 1),
            type(D.fromkeys("a")).__name__,
            bytes.fromhex("01ff"),
            float.fromhex("0x1.8p1"),
            int.from_bytes(b"\x01\x00", "little"),
        )
    return out


class NativeClassmethodTests(unittest.TestCase):
    def test_chain(self):
        for n in (1, 400):
            it = It()
            got = chains(n, it)
            self.assertEqual(got[0], [1, 2, 3])
            self.assertEqual(got[1], [n, 0])
            self.assertEqual(got[2], "Chain")
            self.assertEqual(got[3], [1, 2, 3])
            self.assertEqual(it.seen, n)

    def test_others(self):
        want = ({"a": 0, "b": 0}, {"a": 2, "b": 2}, "D", b"\x01\xff", 3.0, 1)
        for n in (1, 400):
            self.assertEqual(others(n), want)

    def test_errors(self):
        def bad(n):
            caught = 0
            for i in range(n):
                try:
                    itertools.chain.from_iterable(Bad())
                except RuntimeError:
                    caught += 1
                try:
                    bytes.fromhex("zz")
                except ValueError:
                    caught += 1
                try:
                    itertools.chain.from_iterable(5)
                except TypeError:
                    caught += 1
            return caught

        self.assertEqual(bad(300), 900)


if __name__ == "__main__":
    unittest.main()
