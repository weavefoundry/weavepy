"""Observing and changing the levels of a `yield from` chain between items.

Once a chain has passed a value up, its levels may stay collapsed: the next
resume of the outermost level runs the innermost generator directly again.
Anything that reaches an intermediate level in between (its state, frame,
locals, `send`, `throw`, `close`, its release, a tracer) must find it just
as resuming the chain level by level leaves it: suspended in its
`yield from`, with every later item, exception and finalizer unchanged.
"""

import asyncio
import gc
import inspect
import sys
import traceback
import types
import unittest

N = 50


def leaf(n, log=None):
    try:
        for i in range(n):
            yield i
    finally:
        if log is not None:
            log.append("leaf")


def level(depth, n, log=None, gens=None):
    try:
        if depth == 0:
            inner = leaf(n, log)
        else:
            inner = level(depth - 1, n, log, gens)
        if gens is not None:
            gens.append(inner)
        result = yield from inner
        return result
    finally:
        if log is not None:
            log.append(depth)


def started(depth, n=N, items=3, log=None):
    """A chain `depth + 1` levels deep, `items` items in, with its levels
    outermost first (the innermost generator last)."""
    gens = []
    top = level(depth, n, log, gens)
    for i in range(items):
        assert next(top) == i
    return top, [top] + gens


class ParkedChainTest(unittest.TestCase):
    def test_levels_suspended(self):
        top, gens = started(5)
        for g in gens:
            self.assertFalse(g.gi_running)
            self.assertTrue(g.gi_suspended)
            self.assertEqual(inspect.getgeneratorstate(g), inspect.GEN_SUSPENDED)
        for outer, inner in zip(gens, gens[1:]):
            self.assertIs(outer.gi_yieldfrom, inner)
        self.assertIsNone(gens[-1].gi_yieldfrom)
        self.assertEqual(list(top), list(range(3, N)))

    def test_frames_between_items(self):
        top, gens = started(5)
        line = level.__code__.co_firstlineno + 8
        for g in gens[:-1]:
            frame = g.gi_frame
            self.assertEqual(frame.f_code.co_name, "level")
            self.assertEqual(frame.f_lineno, line)
            self.assertIsNone(frame.f_back)
        frames = [g.gi_frame for g in gens[:-1]]
        self.assertEqual(len({f.f_lasti for f in frames}), 1)
        self.assertEqual(next(top), 3)
        # Still the same frames, on the same line.
        self.assertEqual([g.gi_frame for g in gens[:-1]], frames)
        self.assertEqual({f.f_lineno for f in frames}, {line})
        self.assertEqual(list(top), list(range(4, N)))

    def test_locals_of_a_level(self):
        top, gens = started(4)
        mid = gens[2]
        self.assertEqual(mid.gi_frame.f_locals["depth"], 2)
        self.assertIs(mid.gi_frame.f_locals["inner"], gens[3])
        self.assertEqual(next(top), 3)
        self.assertEqual(mid.gi_frame.f_locals["depth"], 2)
        self.assertEqual(list(top), list(range(4, N)))

    def test_next_on_a_level(self):
        top, gens = started(5)
        mid = gens[3]
        self.assertEqual(next(mid), 3)
        self.assertEqual(next(mid), 4)
        self.assertEqual(next(top), 5)
        self.assertEqual(next(gens[1]), 6)
        self.assertEqual(next(top), 7)
        self.assertEqual(list(top), list(range(8, N)))

    def test_next_on_the_innermost(self):
        top, gens = started(5)
        self.assertEqual(next(gens[-1]), 3)
        self.assertEqual(next(top), 4)
        self.assertEqual(next(gens[-1]), 5)
        self.assertEqual(list(top), list(range(6, N)))

    def test_send_to_a_level(self):
        def echo():
            got = None
            while True:
                got = yield got

        def wrap(g):
            return (yield from g)

        inner = echo()
        gens = [inner]
        for _ in range(5):
            gens.insert(0, wrap(gens[0]))
        top = gens[0]
        next(top)
        for i in range(3):
            self.assertEqual(top.send(i), i)
        self.assertEqual(gens[2].send("mid"), "mid")
        self.assertEqual(next(top), None)
        self.assertEqual(top.send(7), 7)
        self.assertEqual(gens[4].send(8), 8)
        self.assertEqual(top.send(9), 9)

    def test_throw_into_a_level(self):
        log = []
        top, gens = started(5, log=log)
        with self.assertRaises(KeyError):
            gens[3].throw(KeyError("x"))
        # The leaf and two levels finished, innermost first.
        self.assertEqual(log, ["leaf", 0, 1, 2])
        self.assertEqual(inspect.getgeneratorstate(gens[3]), inspect.GEN_CLOSED)
        self.assertEqual(inspect.getgeneratorstate(gens[2]), inspect.GEN_SUSPENDED)
        # The level above finds its finished delegate exhausted, and the
        # rest of the chain finishes in turn.
        with self.assertRaises(StopIteration):
            next(top)
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])

    def test_throw_caught_by_a_level(self):
        def catching(g):
            try:
                yield from g
            except KeyError:
                yield "caught"
            yield "after"

        inner = leaf(N)
        mid = catching(inner)
        top = catching(catching(mid))
        self.assertEqual([next(top) for _ in range(3)], [0, 1, 2])
        self.assertEqual(mid.throw(KeyError), "caught")
        self.assertEqual(inspect.getgeneratorstate(inner), inspect.GEN_CLOSED)
        self.assertEqual(next(top), "after")
        self.assertEqual(next(top), "after")
        self.assertEqual(next(top), "after")
        with self.assertRaises(StopIteration):
            next(top)

    def test_close_a_level(self):
        log = []
        top, gens = started(5, log=log)
        gens[3].close()
        self.assertEqual(log, ["leaf", 0, 1, 2])
        # The level above finds its delegate exhausted (its return value
        # `None`) and finishes in turn, as do the rest.
        with self.assertRaises(StopIteration):
            next(top)
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])

    def test_close_the_top(self):
        log = []
        top, gens = started(5, log=log)
        top.close()
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])
        for g in gens:
            self.assertEqual(inspect.getgeneratorstate(g), inspect.GEN_CLOSED)

    def test_release_runs_finalizers_at_once(self):
        log = []
        top, gens = started(5, log=log)
        del gens
        self.assertEqual(log, [])
        del top
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])

    def test_release_of_a_shared_level(self):
        log = []
        top, gens = started(5, log=log)
        mid = gens[2]
        del gens
        del top
        # The levels above `mid` finish at their `yield from`, which
        # closes `mid`: everything finishes.
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])
        self.assertEqual(inspect.getgeneratorstate(mid), inspect.GEN_CLOSED)

    def test_collect_between_items(self):
        top, gens = started(5)
        gc.collect()
        self.assertTrue(all(g.gi_suspended for g in gens))
        self.assertEqual(next(top), 3)
        gc.collect()
        self.assertEqual(list(top), list(range(4, N)))

    def test_cycle_through_a_level(self):
        log = []
        top, gens = started(5, log=log)
        holder = []
        holder.append(gens[2])
        # A cycle: the innermost level's local refers back to the chain.
        gens[-1].gi_frame.f_locals  # (a read only)
        cycle = [top, holder]
        holder.append(cycle)
        del top, gens, holder, cycle
        gc.collect()
        self.assertEqual(log, ["leaf", 0, 1, 2, 3, 4, 5])

    def test_raise_after_parking(self):
        def failing(n):
            for i in range(n):
                yield i
            raise ValueError("deep")

        def wrap(depth):
            if depth == 0:
                yield from failing(5)
            else:
                yield from wrap(depth - 1)

        g = wrap(4)
        out = []
        try:
            for x in g:
                out.append(x)
        except ValueError as e:
            names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        else:
            self.fail("no ValueError")
        self.assertEqual(out, list(range(5)))
        self.assertEqual(names, ["test_raise_after_parking"] + ["wrap"] * 5 + ["failing"])

    def test_stop_iteration_values(self):
        def counting(n):
            for i in range(n):
                yield i
            return n

        def adding(depth, n):
            if depth == 0:
                got = yield from counting(n)
            else:
                got = yield from adding(depth - 1, n)
            yield ("level", depth, got)
            return got + 1

        out = list(adding(4, 6))
        self.assertEqual(out[:6], list(range(6)))
        self.assertEqual(
            out[6:],
            [("level", 0, 6), ("level", 1, 7), ("level", 2, 8), ("level", 3, 9), ("level", 4, 10)],
        )

    def test_two_consumers_share_a_chain(self):
        def wrap(g):
            yield from g

        shared = level(4, N)
        a = wrap(shared)
        b = wrap(shared)
        got = []
        for _ in range(10):
            got.append(next(a))
            got.append(next(b))
        self.assertEqual(got, list(range(20)))
        self.assertEqual(list(a), list(range(20, N)))
        self.assertEqual(list(b), [])

    def test_trace_after_parking(self):
        top, gens = started(4)
        events = []

        def tracer(frame, event, arg):
            if event in ("call", "return"):
                events.append((event, frame.f_code.co_name))
            return tracer

        sys.settrace(tracer)
        try:
            value = next(top)
        finally:
            sys.settrace(None)
        self.assertEqual(value, 3)
        self.assertEqual(
            events,
            [("call", "level")] * 5
            + [("call", "leaf"), ("return", "leaf")]
            + [("return", "level")] * 5,
        )
        self.assertEqual(list(top), list(range(4, N)))

    def test_generator_expression_consumer(self):
        top, gens = started(6)
        self.assertEqual(sum(x for x in top if x % 2), sum(i for i in range(3, N) if i % 2))

    def test_coroutine_chain_between_sends(self):
        @types.coroutine
        def tick(i):
            return (yield i)

        async def leaf_coro(n):
            total = 0
            for i in range(n):
                total += await tick(i)
            return total

        async def wrap(depth, n, coros):
            if depth == 0:
                inner = leaf_coro(n)
            else:
                inner = wrap(depth - 1, n, coros)
            coros.append(inner)
            return await inner + 1

        coros = []
        coro = wrap(5, N, coros)
        self.assertEqual(coro.send(None), 0)
        self.assertEqual(coro.send(1), 1)
        mid = coros[-3]
        self.assertEqual(inspect.getcoroutinestate(mid), inspect.CORO_SUSPENDED)
        self.assertIsNotNone(mid.cr_await)
        self.assertEqual(mid.cr_frame.f_code.co_name, "wrap")
        value = 2
        try:
            while True:
                value = coro.send(1)
        except StopIteration as stop:
            result = stop.value
        self.assertEqual(value, N - 1)
        self.assertEqual(result, N + 6)

    def test_asyncio_cancel_nested(self):
        log = []

        async def inner():
            try:
                while True:
                    await asyncio.sleep(0)
            finally:
                log.append("inner")

        async def wrap(depth):
            try:
                if depth == 0:
                    await inner()
                else:
                    await wrap(depth - 1)
            finally:
                log.append(depth)

        async def main():
            task = asyncio.ensure_future(wrap(5))
            for _ in range(20):
                await asyncio.sleep(0)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
            stack = []
            return stack

        asyncio.run(main())
        self.assertEqual(log, ["inner", 0, 1, 2, 3, 4, 5])

    def test_asyncio_get_stack(self):
        async def inner(ready):
            ready.set_result(None)
            await asyncio.sleep(3600)

        async def wrap(depth, ready):
            if depth == 0:
                await inner(ready)
            else:
                await wrap(depth - 1, ready)

        async def main():
            loop = asyncio.get_running_loop()
            ready = loop.create_future()
            task = asyncio.ensure_future(wrap(4, ready))
            await ready
            for _ in range(5):
                await asyncio.sleep(0)
            names = [f.f_code.co_name for f in task.get_stack(limit=100)]
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
            return names

        self.assertEqual(asyncio.run(main())[:1], ["wrap"])


if __name__ == "__main__":
    unittest.main()
