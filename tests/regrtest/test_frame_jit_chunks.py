"""A body the frame JIT compiles in chunks behaves as one compiled whole.

The workload runs in a subprocess with tiny chunks, so most jumps, loop back
edges, exception handlers and fused instruction sequences cross from one
chunk into another, and its output must match an ordinary run's. (Tier 2
is off in both, so the frame JIT takes the loops.)
"""

import os
import subprocess
import sys
import unittest

N = 3000


def branches(n):
    total = 0
    for i in range(n):
        if i % 3 == 0:
            total += i
        elif i % 3 == 1:
            total -= 1
        else:
            total ^= i
        while total > 1000:
            total //= 7
    return total


def handlers(n):
    caught = []
    for i in range(n):
        try:
            if i % 5 == 0:
                raise ValueError(i)
            caught.append({}.get(i, i) * 2)
        except ValueError as e:
            caught.append(-e.args[0])
        finally:
            caught.append(0)
    return sum(caught), len(caught)


def gen(n):
    for i in range(n):
        yield i * i


def generators(n):
    return sum(gen(n)) + sum(x for x in range(n) if x % 2)


def closures(n):
    def add(k):
        return lambda x: x + k

    fs = [add(k) for k in range(8)]
    return sum(f(i) for i in range(n) for f in fs)


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def moved(self, dx):
        return Point(self.x + dx, self.y - dx)


def objects(n):
    p = Point(0, 0)
    seen = {}
    for i in range(n):
        p = p.moved(1)
        key = (p.x % 7, p.y % 5)
        seen[key] = seen.get(key, 0) + 1
        a, b = key
        if a == b:
            del seen[key]
    return p.x, p.y, sorted(seen.items())


def workload():
    for _ in range(3):
        out = [branches(N), handlers(N), generators(N), closures(N), objects(N)]
    print(out)


class ChunksTest(unittest.TestCase):
    def run_with(self, **env):
        r = subprocess.run(
            [sys.executable, __file__, "run"],
            env=dict(os.environ, PYTHONHASHSEED="0", WEAVEPY_TIER2="0", **env),
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout

    def test_tiny_chunks_match(self):
        whole = self.run_with(WEAVEPY_FRAME_JIT_CHUNK="0")
        chunked = self.run_with(WEAVEPY_FRAME_JIT_CHUNK="40", WEAVEPY_FRAME_JIT_HOT="2")
        self.assertEqual(whole, chunked)


if __name__ == "__main__":
    if sys.argv[1:] == ["run"]:
        workload()
    else:
        unittest.main()
