"""Instructions compiled code now runs itself instead of leaving them.

Generator expressions made in a loop (closures, `MAKE_FUNCTION`,
`SET_FUNCTION_ATTRIBUTE`, `LOAD_COMMON_CONSTANT`), cell stores, starred
unpacking, set and string iteration, tuple and list concatenation, list
slice stores, and an inlined comprehension's variable in a function with
cells must behave as the interpreter runs them, errors included.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


def genexprs(pool, n):
    out = 0
    for k in range(n):
        out += len(tuple(pool[i] for i in range(k % 3)))
        out += all(p >= 0 for p in pool) + any(p > k for p in pool)
    return out


def cell_stores(n):
    total = 0
    acc = 0

    def read():
        return acc

    for i in range(n):
        acc = i
        total += read()
    return total


def starred(seqs):
    out = []
    for s in seqs:
        a, *b, c = s
        out.append((a, b, c))
    return out


def starred_short(s, n):
    for _ in range(n):
        a, *b, c = s
    return a, b, c


def iterate(n, things):
    count = 0
    for _ in range(n):
        for x in things:
            count += 1
    return count


def mutate_set(s):
    for x in s:
        s.add(x + 100)


def concat(n):
    t, l = (), []
    for i in range(n):
        t = t + (i,)
        l = l + [i]
    return t, l


def slice_stores(n):
    xs = list(range(10))
    for i in range(n):
        k = i % 10
        xs[k:] = xs[k + 1:] + xs[k:k + 1]
        xs[:1] = (xs[0],)
        xs[-3:-1] = [xs[-2], xs[-3]]
    return xs


def comprehension_with_cells(values, n):
    s = "outer"
    total = 0
    for _ in range(n):
        picked = [s for s in values if s > 1]
        total += len(picked) + len([x for x in values if (lambda: x)() > 0])
    return total, s


class MoreOpsTest(unittest.TestCase):
    def test_genexprs(self):
        pool = [3, 1, 2]
        expect = sum(len(tuple(pool[i] for i in range(k % 3))) + 1 + (3 > k)
                     for k in range(N))
        self.assertEqual(genexprs(pool, N), expect)

    def test_cell_stores(self):
        self.assertEqual(cell_stores(N), sum(range(N)))

    def test_starred(self):
        seqs = [(1, 2), [1, 2, 3], (1, 2, 3, 4, 5), [9, 8]] * 800
        out = starred(seqs)
        self.assertEqual(out[:4], [(1, [], 2), (1, [2], 3), (1, [2, 3, 4], 5), (9, [], 8)])
        self.assertEqual(len(out), len(seqs))
        with self.assertRaises(ValueError):
            starred_short((1,), N)
        self.assertEqual(starred_short("xy", N), ("x", [], "y"))

    def test_iterate(self):
        self.assertEqual(iterate(N, {1, 2, 3}), 3 * N)
        self.assertEqual(iterate(N, frozenset("abcd")), 4 * N)
        self.assertEqual(iterate(N, "hello"), 5 * N)
        self.assertEqual(iterate(N, b"hey"), 3 * N)
        with self.assertRaises(RuntimeError):
            mutate_set({1, 2, 3})

    def test_concat(self):
        t, l = concat(N)
        self.assertEqual(t, tuple(range(N)))
        self.assertEqual(l, list(range(N)))

    def test_slice_stores(self):
        expect = list(range(10))
        for i in range(N):
            k = i % 10
            expect[k:] = expect[k + 1:] + expect[k:k + 1]
            expect[:1] = (expect[0],)
            expect[-3:-1] = [expect[-2], expect[-3]]
        self.assertEqual(slice_stores(N), expect)

    def test_comprehension_with_cells(self):
        self.assertEqual(comprehension_with_cells([0, 1, 2, 3], N), (5 * N, "outer"))

    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
