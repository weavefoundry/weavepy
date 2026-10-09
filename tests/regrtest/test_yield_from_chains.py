"""Deep `yield from` and `await` chains resumed many times.

A resumed chain may run its innermost generator directly, leaving the
generators between parked in their delegations. Everything Python can see
must stay as it is when each level resumes in turn: values, return values,
exceptions caught by the levels, the frame spine (`f_back`, stack
summaries), the levels' running state, and the recursion limit.
"""

import inspect
import sys
import traceback
import types
import unittest

N = 400


def leaf(n):
    for i in range(n):
        yield i


def chain(depth, n):
    if depth == 0:
        yield from leaf(n)
    else:
        yield from chain(depth - 1, n)


def summing(depth, n):
    if depth == 0:
        for i in range(n):
            yield i
        return n
    got = yield from summing(depth - 1, n)
    return got + 1


class Tree:
    def __init__(self, left, value, right):
        self.left, self.value, self.right = left, value, right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


class YieldFromChainsTest(unittest.TestCase):
    def test_values(self):
        for depth in (1, 2, 5, 20):
            self.assertEqual(list(chain(depth, N)), list(range(N)))
            self.assertEqual(sum(chain(depth, N)), sum(range(N)))

    def test_return_values(self):
        def outer():
            result = yield from summing(6, N)
            yield ("result", result)

        out = list(outer())
        self.assertEqual(out[:-1], list(range(N)))
        self.assertEqual(out[-1], ("result", N + 6))

    def test_tree(self):
        tree = build(0, 1 << 9)
        self.assertEqual(list(tree), list(range(1 << 9)))
        it = iter(tree)
        pairs = [(a, next(it, None)) for a in it]
        self.assertEqual(pairs[:2], [(0, 1), (2, 3)])

    def test_exception_caught_by_level(self):
        def boom(n):
            for i in range(n):
                yield i
            raise KeyError("boom")

        def catcher(depth):
            if depth == 0:
                try:
                    yield from boom(N)
                except KeyError as e:
                    yield ("caught", e.args[0])
                return
            try:
                yield from catcher(depth - 1)
            finally:
                yield ("finally", depth)

        out = list(catcher(4))
        self.assertEqual(out[:N], list(range(N)))
        self.assertEqual(out[N:], [("caught", "boom")] + [("finally", d) for d in (1, 2, 3, 4)])

    def test_exception_propagates_with_traceback(self):
        def failing(n):
            yield from range(n)
            raise ValueError("deep")

        def level(depth):
            if depth == 0:
                yield from failing(N)
            else:
                yield from level(depth - 1)

        try:
            for _ in level(5):
                pass
        except ValueError as e:
            names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        else:
            self.fail("no ValueError")
        self.assertEqual(names, ["test_exception_propagates_with_traceback"] + ["level"] * 6 + ["failing"])

    def test_frame_spine(self):
        seen = []

        def inner(n):
            for i in range(n):
                if i == n - 1:
                    f = sys._getframe()
                    names = []
                    while f is not None and f.f_code.co_name != "test_frame_spine":
                        names.append(f.f_code.co_name)
                        f = f.f_back
                    seen.append(names)
                    seen.append([fs.name for fs in traceback.extract_stack()][-6:])
                yield i

        def mid(depth, n):
            if depth == 0:
                yield from inner(n)
            else:
                yield from mid(depth - 1, n)

        self.assertEqual(list(mid(3, N)), list(range(N)))
        self.assertEqual(seen[0], ["inner", "mid", "mid", "mid", "mid"])
        self.assertEqual(seen[1][-5:], ["mid", "mid", "mid", "mid", "inner"])

    def test_levels_are_running(self):
        gens = []
        states = []

        def inner(n):
            for i in range(n):
                if i == n - 1:
                    for g in gens:
                        states.append((g.gi_running, inspect.getgeneratorstate(g), g.gi_yieldfrom))
                        with self.assertRaisesRegex(ValueError, "already executing"):
                            next(g)
                        with self.assertRaisesRegex(ValueError, "already executing"):
                            g.send(None)
                    frame = gens[1].gi_frame
                    states.append(frame.f_code.co_name)
                yield i

        def mid(depth, n):
            if depth == 0:
                yield from inner(n)
            else:
                g = mid(depth - 1, n)
                gens.append(g)
                yield from g

        top = mid(3, N)
        gens.append(top)
        self.assertEqual(list(top), list(range(N)))
        self.assertEqual(states[:4], [(True, inspect.GEN_RUNNING, None)] * 4)
        self.assertEqual(states[4], "mid")

    def test_levels_suspended_between_items(self):
        top = chain(4, N)
        for _ in range(N // 2):
            next(top)
        self.assertFalse(top.gi_running)
        self.assertEqual(inspect.getgeneratorstate(top), inspect.GEN_SUSPENDED)
        level = top
        depth = 0
        while level.gi_yieldfrom is not None:
            self.assertEqual(inspect.getgeneratorstate(level), inspect.GEN_SUSPENDED)
            level = level.gi_yieldfrom
            depth += 1
        self.assertEqual(depth, 5)
        self.assertEqual(level.gi_frame.f_code.co_name, "leaf")
        top.close()
        self.assertEqual(inspect.getgeneratorstate(top), inspect.GEN_CLOSED)

    def test_throw_and_close_reach_innermost(self):
        log = []

        def inner():
            try:
                for i in range(N):
                    yield i
            except KeyError:
                log.append("inner caught")
                yield "recovered"
            finally:
                log.append("inner closed")

        def level(depth):
            if depth == 0:
                yield from inner()
            else:
                yield from level(depth - 1)

        g = level(4)
        for _ in range(N // 2):
            next(g)
        self.assertEqual(g.throw(KeyError), "recovered")
        g.close()
        self.assertEqual(log, ["inner caught", "inner closed"])

    def test_recursion_limit(self):
        def headroom():
            def count(k):
                try:
                    return count(k + 1)
                except RecursionError:
                    return k

            return count(0)

        def probing(n):
            for i in range(n):
                yield i
            yield headroom()

        def deep(depth):
            if depth == 0:
                yield from probing(N)
            else:
                yield from deep(depth - 1)

        old = sys.getrecursionlimit()
        try:
            sys.setrecursionlimit(400)
            shallow = list(deep(0))[-1]
            deeper = list(deep(120))[-1]
            self.assertLessEqual(abs(shallow - deeper - 120), 2)
        finally:
            sys.setrecursionlimit(old)

    def test_coroutine_chain(self):
        @types.coroutine
        def tick(i):
            got = yield i
            return got

        async def leaf_coro(n):
            total = 0
            for i in range(n):
                total += await tick(i)
            return total

        async def level(depth, n):
            if depth == 0:
                return await leaf_coro(n)
            return await level(depth - 1, n) + 1

        coro = level(5, N)
        sent = []
        value = coro.send(None)
        try:
            while True:
                sent.append(value)
                value = coro.send(value * 2)
        except StopIteration as stop:
            result = stop.value
        self.assertEqual(sent, list(range(N)))
        self.assertEqual(result, 2 * sum(range(N)) + 5)

    def test_send_values_pass_through(self):
        def echo():
            got = None
            while True:
                got = yield got
                if got == "stop":
                    return "done"

        def level(depth):
            if depth == 0:
                return (yield from echo())
            return (yield from level(depth - 1))

        g = level(5)
        next(g)
        for i in range(N):
            self.assertEqual(g.send(i), i)
        with self.assertRaises(StopIteration) as cm:
            g.send("stop")
        self.assertEqual(cm.exception.value, "done")


if __name__ == "__main__":
    unittest.main()
