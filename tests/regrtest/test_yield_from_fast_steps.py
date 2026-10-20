"""`yield from` and `await` of simple generators stepped in place.

A `SEND` to a simple generator, or the resume of a collapsed chain whose
innermost generator is a simple one, may run that generator's body to its
next yield without an activation: the value passes every level by, and a
return lands in the innermost level's `SEND`. Values, return values,
`StopIteration`, the levels' state and frames, and every consumer
(`for`, `next()`, `send(None)`, `sum()`, coroutine drivers, asyncio) must
see exactly what resuming each level in turn shows.
"""

import asyncio
import inspect
import types
import unittest

N = 40


def counting(n):
    for i in range(n):
        yield i
    return n


def relay(depth, n, log=None):
    if depth == 0:
        got = yield from counting(n)
    else:
        got = yield from relay(depth - 1, n, log)
    if log is not None:
        log.append((depth, got))
    return got + 1


def twice(n):
    a = yield from counting(n)
    b = yield from counting(n)
    return a + b


class YieldFromFastStepsTest(unittest.TestCase):
    def test_values_and_returns(self):
        for depth in (0, 1, 3, 12):
            log = []
            g = relay(depth, N, log)
            self.assertEqual(list(g), list(range(N)))
            self.assertEqual(log, [(d, N + d) for d in range(depth + 1)])

    def test_return_value_reaches_consumer(self):
        for depth in (0, 2, 6):
            g = relay(depth, 3)
            self.assertEqual([next(g), next(g), next(g)], [0, 1, 2])
            with self.assertRaises(StopIteration) as cm:
                next(g)
            self.assertEqual(cm.exception.value, 3 + depth + 1)

    def test_sequential_delegations(self):
        def outer():
            total = yield from twice(5)
            yield ("total", total)

        for _ in range(50):
            self.assertEqual(list(outer()), list(range(5)) * 2 + [("total", 10)])

    def test_consumers(self):
        for depth in (1, 4):
            self.assertEqual(sum(relay(depth, N)), sum(range(N)))
            g = relay(depth, N)
            self.assertEqual([next(g, None) for _ in range(N + 2)], list(range(N)) + [None, None])
            g = relay(depth, N)
            out = []
            try:
                while True:
                    out.append(g.send(None))
            except StopIteration as e:
                self.assertEqual(e.value, N + depth + 1)
            self.assertEqual(out, list(range(N)))

    def test_send_values_mixed(self):
        def echo():
            got = None
            for _ in range(N):
                got = yield got
            return "done"

        def wrap(depth):
            if depth == 0:
                return (yield from echo())
            return (yield from wrap(depth - 1))

        g = wrap(4)
        self.assertIsNone(next(g))
        seen = []
        for i in range(N - 1):
            seen.append(g.send(i if i % 3 else None))
        self.assertEqual(seen, [i if i % 3 else None for i in range(N - 1)])
        with self.assertRaises(StopIteration) as cm:
            g.send(None)
        self.assertEqual(cm.exception.value, "done")

    def test_levels_between_items(self):
        gens = []

        def level(depth):
            if depth == 0:
                inner = counting(N)
            else:
                inner = level(depth - 1)
            gens.append(inner)
            yield from inner

        top = level(4)
        for _ in range(5):
            next(top)
        self.assertEqual(len(gens), 5)
        for g in [top] + gens[:-1]:
            self.assertTrue(g.gi_suspended)
            self.assertIsNotNone(g.gi_yieldfrom)
            self.assertEqual(g.gi_frame.f_code.co_name, "level")
        inner = gens[-1]
        self.assertEqual(inner.gi_frame.f_locals["i"], 4)
        self.assertEqual(next(top), 5)
        self.assertEqual(next(inner), 6)
        self.assertEqual(list(top), list(range(7, N)))

    def test_close_and_throw_reach_the_simple_generator(self):
        log = []

        def guarded(n):
            try:
                for i in range(n):
                    yield i
            finally:
                log.append("closed")

        def wrap(depth):
            if depth == 0:
                yield from guarded(N)
            else:
                yield from wrap(depth - 1)

        g = wrap(3)
        self.assertEqual([next(g) for _ in range(4)], [0, 1, 2, 3])
        g.close()
        self.assertEqual(log, ["closed"])
        g = wrap(3)
        next(g)
        with self.assertRaises(KeyError):
            g.throw(KeyError)
        self.assertEqual(log, ["closed", "closed"])

    def test_await_simple_awaitables(self):
        class Tick:
            def __await__(self):
                got = yield "tick"
                return got

        async def ticking(n):
            total = 0
            for i in range(n):
                total += await Tick() or 0
            return total

        async def wrap(depth, n):
            if depth == 0:
                return await ticking(n)
            return await wrap(depth - 1, n)

        for depth in (0, 3):
            coro = wrap(depth, N)
            sent = []
            value = coro.send(None)
            try:
                k = 0
                while True:
                    sent.append(value)
                    k += 1
                    value = coro.send(None if k % 2 else k)
            except StopIteration as e:
                result = e.value
            self.assertEqual(sent, ["tick"] * N)
            self.assertEqual(result, sum(k for k in range(1, N + 1) if k % 2 == 0))

    def test_types_coroutine_chain(self):
        @types.coroutine
        def step():
            yield

        async def inner(n):
            for _ in range(n):
                await step()
            return n

        async def outer(n):
            return await inner(n) + 1

        coro = outer(N)
        count = 0
        try:
            while True:
                self.assertIsNone(coro.send(None))
                count += 1
                if count == N // 2:
                    self.assertEqual(inspect.getcoroutinestate(coro), inspect.CORO_SUSPENDED)
                    self.assertIsNotNone(coro.cr_await)
        except StopIteration as e:
            self.assertEqual(e.value, N + 1)
        self.assertEqual(count, N)

    def test_asyncio_sleep_zero(self):
        async def worker(k, log):
            for i in range(10):
                await asyncio.sleep(0)
                log.append((k, i))

        async def main():
            log = []
            await asyncio.gather(*(worker(k, log) for k in range(3)))
            return log

        log = asyncio.run(main())
        self.assertEqual(sorted(log), [(k, i) for k in range(3) for i in range(10)])
        self.assertEqual([i for k, i in log if k == 0], list(range(10)))


if __name__ == "__main__":
    unittest.main()
