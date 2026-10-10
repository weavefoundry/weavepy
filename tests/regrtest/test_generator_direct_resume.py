"""Compiled generator bodies resumed straight from compiled consumers: a
`FOR_ITER` or `SEND` in native code runs the generator's native code
itself, and its yield or return comes back to the consumer's native code
(see `run_switched_directly` in the VM's frame JIT).

Each case warms a consumer and a generator that fast steps don't take
(a call or a handler in the body) until both compile, then checks values,
exhaustion, raises, return values through `yield from` and `await`,
frames seen from inside the generator, finalizers, and recursion limits.
"""

import sys
import unittest

WARM = 3000


def double(x):
    return x * 2


def calls(n):
    for i in range(n):
        yield double(i)


def guarded(xs):
    for x in xs:
        try:
            yield 10 // x
        except ZeroDivisionError:
            yield None


def consume(gen):
    total = 0
    for x in gen:
        total += x
    return total


def inner(n):
    for i in range(n):
        yield double(i)
    return n


def outer(n):
    got = yield from inner(n)
    yield got


async def leaf(x):
    return double(x)


async def node(x):
    return await leaf(x) + await leaf(x + 1)


def drive(coro):
    try:
        coro.send(None)
    except StopIteration as e:
        return e.value


class DirectResumeTests(unittest.TestCase):
    def test_values_and_exhaustion(self):
        for _ in range(WARM):
            consume(calls(5))
        self.assertEqual(consume(calls(10)), sum(2 * i for i in range(10)))
        self.assertEqual(consume(calls(0)), 0)
        self.assertEqual(list(calls(3)), [0, 2, 4])

    def test_raise_inside(self):
        for _ in range(WARM):
            list(guarded([1, 2, 0, 5]))
        self.assertEqual(list(guarded([1, 0, 2])), [10, None, 5])

        def bad(n):
            for i in range(n):
                yield double(i)
            raise KeyError("end")

        for _ in range(WARM):
            try:
                consume(bad(3))
            except KeyError:
                pass
        with self.assertRaises(KeyError):
            consume(bad(3))
        with self.assertRaises(RuntimeError):
            # PEP 479: a StopIteration escaping the body.
            def stopper():
                yield double(1)
                raise StopIteration

            consume(stopper())

    def test_yield_from_return_value(self):
        for _ in range(WARM):
            list(outer(3))
        self.assertEqual(list(outer(3)), [0, 2, 4, 3])

    def test_await_chain(self):
        for i in range(WARM):
            drive(node(i))
        self.assertEqual(drive(node(5)), 22)

    def test_frames_from_inside(self):
        def where():
            for _ in range(2):
                f = sys._getframe()
                yield (f.f_code.co_name, f.f_back.f_code.co_name if f.f_back else None)

        def run():
            out = []
            for item in where():
                out.append(item)
            return out

        for _ in range(WARM):
            run()
        self.assertEqual(run(), [("where", "run"), ("where", "run")])

    def test_finalizer_on_exhaustion(self):
        log = []

        class Tracked:
            def __del__(self):
                log.append("del")

        def holder():
            t = Tracked()
            yield double(1)
            del t
            log.append("after")
            yield double(2)

        for _ in range(WARM):
            consume(holder())
        log.clear()
        for x in holder():
            log.append(x)
        self.assertEqual(log, [2, "del", "after", 4])

    def test_recursion_limit(self):
        def nest(n):
            if n:
                yield from nest(n - 1)
            yield double(n)

        for _ in range(200):
            consume(nest(20))
        limit = sys.getrecursionlimit()
        try:
            sys.setrecursionlimit(200)
            with self.assertRaises(RecursionError):
                consume(nest(400))
        finally:
            sys.setrecursionlimit(limit)
        self.assertEqual(consume(nest(50)), sum(2 * i for i in range(51)))

    def test_close_and_throw_midway(self):
        def counting():
            try:
                for i in range(10):
                    yield double(i)
            finally:
                log.append("closed")

        log = []
        for _ in range(WARM):
            g = counting()
            for x in g:
                if x > 4:
                    break
            g.close()
        log.clear()
        g = counting()
        for x in g:
            if x > 4:
                break
        g.close()
        self.assertEqual(log, ["closed"])
        g = counting()
        next(g)
        with self.assertRaises(ValueError):
            g.throw(ValueError)
        self.assertEqual(log, ["closed", "closed"])


if __name__ == "__main__":
    unittest.main()
