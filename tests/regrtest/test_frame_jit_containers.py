"""The frame JIT's container shortcuts keep Python's semantics.

Each function runs long enough to be compiled, then meets the shapes its
in-place helpers and method kernels hand back to the general paths: keys
that compare equal across types, subclass receivers with overrides,
non-ASCII strings, out-of-range indices, wrong unpack lengths, misses that
raise, and displaced values whose finalizers must run at once.
"""

import os
import subprocess
import sys
import unittest

N = 3000


class Key:
    """Equal to (and hashed like) the string it wraps."""

    def __init__(self, s):
        self.s = s

    def __hash__(self):
        return hash(self.s)

    def __eq__(self, other):
        return other == self.s or (isinstance(other, Key) and other.s == self.s)


class Loud:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Loud.log.append(self.name)


class MyList(list):
    def append(self, x):
        super().append(("wrapped", x))


def method_calls(n, xs, d, s, text):
    out = []
    for i in range(n):
        xs.append(i)
        xs.pop()
        out.append(d.get("a"))
        out.append(d.get("zz", -1))
        s.add(i & 7)
        s.discard(i & 7)
        out.append(text.startswith("ab"))
        out.append(text.endswith("yz"))
        out.append(text.isdigit())
        out.append(text.find("c"))
        out.append(text.strip())
        out.append(text.upper())
    return out


def subscripts(n, d, xs, t, text, key):
    total = 0
    for i in range(n):
        total += d[key] + xs[1] + xs[-1] + t[0]
        total += len(text[2])
    return total


def contains(n, item, seq):
    hits = 0
    for i in range(n):
        if item in seq:
            hits += 1
    return hits


def unpacks(n, seq):
    total = 0
    for i in range(n):
        a, b, c = seq
        total += a + b + c
    return total


def stores(n, d, xs, key, value):
    for i in range(n):
        d[key] = value
        xs[0] = value
    return d, xs


def iterate(n, text):
    out = []
    for i in range(n):
        for ch in text:
            out.append(ch)
    return out


def hashes(n, value):
    h = 0
    for i in range(n):
        h = hash(value)
    return h


class FrameJitContainerTests(unittest.TestCase):
    def test_method_kernels(self):
        out = method_calls(N, [1, 2], {"a": 1}, set(), "abcxyz")
        self.assertEqual(out[:8], [1, -1, True, True, False, 2, "abcxyz", "ABCXYZ"])
        # Shapes the kernels decline still run correctly.
        out = method_calls(5, MyList(), {1.0: "f", "a": 2}, {True}, " ¹²³ ")
        self.assertEqual(out[:8], [2, -1, False, False, False, -1, "¹²³", " ¹²³ "])
        self.assertTrue("٣٤".isdigit())
        self.assertTrue("\x1c\x1f ".isspace())
        self.assertEqual("héllo".find("l"), 2)

    def test_list_kernels(self):
        xs = [1, 2]
        for _ in range(N):
            xs.extend(())
        xs.extend(xs)
        self.assertEqual(xs, [1, 2, 1, 2])
        m = MyList()
        for i in range(3):
            m.append(i)
        self.assertEqual(m, [("wrapped", 0), ("wrapped", 1), ("wrapped", 2)])
        empty = []
        with self.assertRaises(IndexError):
            for _ in range(N):
                empty.pop()

    def test_set_kernels(self):
        s = {True}
        for _ in range(N):
            s.add(1)
        self.assertEqual(s, {True})
        self.assertIs(next(iter(s)), True)
        s.discard(1)
        self.assertEqual(s, set())
        with self.assertRaises(KeyError):
            for i in range(N):
                s.add(i)
                s.remove(i)
            s.remove(-5)
        t = {Key("a")}
        for _ in range(N):
            t.add("b")
            t.discard("b")
        t.discard("a")
        self.assertEqual(t, set())

    def test_dict_get_kernel(self):
        d = {Key("k"): 5, 2.0: "two"}
        for _ in range(N):
            v = d.get("k")
        self.assertEqual(v, 5)
        self.assertEqual(d.get(2), "two")

    def test_subscripts(self):
        self.assertEqual(subscripts(4 * N, {"k": 1}, [1, 2, 3], (4,), "abcd", "k"), 4 * N * 11)
        with self.assertRaises(KeyError):
            subscripts(1, {}, [1, 2, 3], (4,), "abcd", "k")
        with self.assertRaises(IndexError):
            subscripts(1, {"k": 1}, [1], (4,), "abcd", "k")
        self.assertEqual(subscripts(2, {Key("k"): 1}, [1, 2, 3], (4,), "héllo", "k"), 22)
        self.assertEqual(subscripts(2, {(1, "a"): 1}, [1, 2, 3], (4,), "abcd", (1, "a")), 22)

    def test_contains(self):
        self.assertEqual(contains(N, "c", "abcd"), N)
        self.assertEqual(contains(N, 3, {1.5, 3.0}), N)
        self.assertEqual(contains(N, "a", {Key("a")}), N)
        self.assertEqual(contains(10, Key("x"), ["y", "x"]), 10)
        with self.assertRaises(TypeError):
            contains(1, 3, "abc")

    def test_unpacks(self):
        self.assertEqual(unpacks(N, (1, 2, 3)), N * 6)
        self.assertEqual(unpacks(N, [1, 2, 3]), N * 6)
        self.assertEqual(unpacks(3, range(1, 4)), 18)
        with self.assertRaises(ValueError):
            unpacks(1, (1, 2))
        with self.assertRaises(ValueError):
            unpacks(1, [1, 2, 3, 4])

    def test_stores(self):
        d, xs = stores(4 * N, {}, [0], "k", 7)
        self.assertEqual((d, xs), ({"k": 7}, [7]))
        d, xs = stores(1, {Key("k"): 0}, [0], "k", 1)
        self.assertEqual(list(d.values()), [1])
        with self.assertRaises(IndexError):
            stores(1, {}, [], "k", 1)
        # A displaced value's finalizer runs before the next statement.
        Loud.log.clear()
        d = {"k": Loud("old")}
        xs = [Loud("old-item")]
        stores(1, d, xs, "k", 0)
        self.assertEqual(Loud.log, ["old", "old-item"])

    def test_iteration(self):
        self.assertEqual(iterate(N, "ab")[:4], ["a", "b", "a", "b"])
        self.assertEqual(iterate(1, "héé"), ["h", "é", "é"])

    def test_hash(self):
        self.assertEqual(hashes(N, -1), -2)
        self.assertEqual(hashes(N, (1, "a")), hash((1, "a")))
        self.assertEqual({(1, "a"): 1}[(1, "a")], 1)
        nan = float("nan")
        self.assertEqual(hashes(3, nan), hash(nan))
        self.assertEqual(hashes(3, (1, 2.5)), hash((1, 2.5)))


    @unittest.skipIf(os.environ.get("WEAVEPY_TIER2") == "0", "already without tier 2")
    def test_without_tier2(self):
        # Tier 2 takes some of these loops first: run them all again with
        # it off, so the frame JIT compiles every one.
        env = dict(os.environ, WEAVEPY_TIER2="0")
        r = subprocess.run(
            [sys.executable, __file__], env=env, capture_output=True, text=True, timeout=120
        )
        self.assertEqual(r.returncode, 0, r.stderr[-2000:])


if __name__ == "__main__":
    unittest.main()
