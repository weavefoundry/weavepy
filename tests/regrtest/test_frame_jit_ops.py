"""The core loop's native code runs globals, attributes, calls, builds,
compares and the other instructions its helpers cover exactly as the
interpreter does.

Each function runs long enough to be compiled (loops by their back edges,
loop-free functions by their calls), then meets the shapes the native code
hands back to the core loop: rebound globals and builtins, raising
callees, heap operands where it expects numbers, inexact int/float
compares, finalizers run by releases, and caches that go stale.
"""

import unittest

N = 3000
CALLS = 30000

SCALE = 3


def scaled(n):
    t = 0
    for i in range(n):
        t += SCALE * i + len("abc") + abs(-i)
    return t


class Slotted:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def add(self, o):
        return Slotted(self.x + o.x, self.y + o.y)


class Plain:
    K = 7

    def __init__(self, v):
        self.v = v
        self.inner = None

    def get(self):
        return self.v + self.K

    @property
    def twice(self):
        return self.v * 2


def wrap(x, y=0):
    return [x]


def pure(x):
    return x + 1


class Dying:
    deaths = 0

    def __del__(self):
        Dying.deaths += 1


class FrameJitOpsTests(unittest.TestCase):
    def test_globals_and_builtins(self):
        global SCALE
        self.assertEqual(scaled(N), scaled(N))
        before = scaled(100)
        SCALE = 5
        try:
            self.assertEqual(scaled(100) - before, 2 * sum(range(100)))
        finally:
            SCALE = 3

    def test_builtin_shadowed_by_global(self):
        g = {"__builtins__": __builtins__}
        exec(
            "def f(n):\n"
            "    t = 0\n"
            "    for i in range(n):\n"
            "        t += len('ab')\n"
            "    return t\n",
            g,
        )
        self.assertEqual(g["f"](N), 2 * N)
        g["len"] = lambda s: 10
        self.assertEqual(g["f"](N), 10 * N)
        del g["len"]
        self.assertEqual(g["f"](N), 2 * N)

    def test_effect_leaf_calls_in_loops(self):
        def run(n):
            xs = []
            for i in range(n):
                ys = wrap(i, 3)
                xs.append((i, ys))
            return xs

        for _ in range(3):
            xs = run(N)
            self.assertEqual(len(xs), N)
            self.assertEqual(xs[-1], (N - 1, [N - 1]))
            self.assertTrue(all(v == (k, [k]) for k, v in enumerate(xs)))

    def test_calls_with_computed_arguments(self):
        def run(n):
            return [wrap(i % 3) for i in range(n)]

        self.assertEqual(run(N)[-3:], [[(N - 3) % 3], [(N - 2) % 3], [(N - 1) % 3]])
        self.assertEqual(sum(pure(i) for i in range(N)), sum(range(1, N + 1)))

    def test_hot_loop_free_functions(self):
        a = Slotted(1.0, 2.0)
        b = Slotted(0.5, 0.25)
        c = a
        for _ in range(CALLS):
            c = a.add(b)
        self.assertEqual((c.x, c.y), (1.5, 2.25))
        p = Plain(3)
        t = 0
        for _ in range(CALLS):
            t += p.get() + p.twice
        self.assertEqual(t, CALLS * 16)
        # A cached class attribute changes under compiled code.
        Plain.K = 8
        try:
            self.assertEqual(p.get(), 11)
        finally:
            Plain.K = 7

    def test_raising_builtin_in_loop(self):
        def run(n):
            caught = 0
            for i in range(n):
                try:
                    int("x" if i % 7 == 0 else "1")
                except ValueError:
                    caught += 1
            return caught

        self.assertEqual(run(N), len(range(0, N, 7)))

    def test_list_methods_and_builds(self):
        def run(n):
            xs = []
            for i in range(n):
                xs.append((i, i + 1))
                if i % 3 == 0:
                    xs.pop()
                t = [i, i * 2]
                xs.append(t[1])
            return xs

        xs = run(N)
        self.assertEqual(len(xs), N + N - len(range(0, N, 3)))
        self.assertEqual(xs[-1], 2 * (N - 1))

    def test_attribute_stores_and_chains(self):
        def run(n):
            o = Plain(0)
            o.inner = Plain(0)
            for i in range(n):
                o.inner.v = i
                o.v = o.inner.v + 1
            return o.v, o.inner.v

        self.assertEqual(run(N), (N, N - 1))

    def test_stores_release_finalizers(self):
        def run(n):
            o = Plain(None)
            xs = [None]
            for i in range(n):
                o.v = Dying()
                xs[0] = Dying()
            return o

        Dying.deaths = 0
        run(N)
        self.assertEqual(Dying.deaths, 2 * N)

    def test_closures(self):
        def make(k):
            def inner(n):
                t = 0
                for i in range(n):
                    t += k + i
                return t

            return inner

        self.assertEqual(make(5)(N), 5 * N + sum(range(N)))

    def test_mixed_compares(self):
        def run(xs, ys):
            r = []
            for x in xs:
                for y in ys:
                    r.append((x < y, x <= y, x == y, x != y, x > y, x >= y))
            return r

        big = 2**53 + 1
        nums = [0, 1, -1, 0.5, -0.0, 1.0, big, float(2**53), float("nan"), float("inf")]
        expect = run(nums, nums)
        for _ in range(40):
            self.assertEqual(run(nums, nums), expect)
        self.assertEqual(run([big], [float(2**53)]), [(False, False, False, True, True, True)])
        words = ["a", "b", "ab", "", "é", "z"]
        expect = run(words, words)
        for _ in range(40):
            self.assertEqual(run(words, words), expect)

    def test_binary_fallbacks(self):
        def run(a, b, n):
            r = None
            for _ in range(n):
                r = a + b
            return r

        self.assertEqual(run("x", "y", N), "xy")
        self.assertEqual(run(1, 2, N), 3)
        self.assertEqual(run(2**62, 2**62, N), 2**63)
        self.assertEqual(run([1], [2], N), [1, 2])
        self.assertEqual(run(1, 2.5, N), 3.5)

    def test_unary(self):
        def run(xs):
            r = []
            for x in xs:
                r.append((-x, +x, not x, ~int(x)))
            return r

        xs = [0, 1, -(2**63), 2.5, -0.0, True, 2**63 - 1]
        expect = [(-x, +x, not x, ~int(x)) for x in xs]
        for _ in range(500):
            self.assertEqual(run(xs), expect)

        def nots(xs):
            return [not x for x in xs]

        for _ in range(500):
            self.assertEqual(nots([None, "", "a", 0.0, []]), [True, True, False, True, True])

    def test_string_indexing(self):
        def run(s, n):
            out = []
            for i in range(n):
                out.append(s[i % len(s)] + s[-1])
            return "".join(out)

        self.assertEqual(run("abc", 6), "acbcccacbccc")
        self.assertEqual(run("aé", 4), "aéééaééé")
        for _ in range(3):
            self.assertEqual(len(run("hello", N)), 2 * N)

        def bad(s, n):
            caught = 0
            for i in range(n):
                try:
                    s[i]
                except IndexError:
                    caught += 1
            return caught

        self.assertEqual(bad("ab", N), N - 2)

    def test_iteration_shapes(self):
        def run(n):
            t = 0
            for i in range(n):
                for k in {"a": 1, "b": 2}:
                    t += len(k)
                for v in (1, 2):
                    t += v
            return t

        self.assertEqual(run(N), 5 * N)


if __name__ == "__main__":
    unittest.main()
