"""A finished frame leaves its function's closure for the next call.

A frame with only free variables shares its vector of cells with its function.
Each case below finishes such a frame one way and calls the function again.
"""

import asyncio
import unittest


def closure_gen():
    x = 1

    def gen():
        yield x
        yield x + 1

    return gen


def closure_catching_gen():
    x = 1

    def gen():
        try:
            yield x
        except ValueError:
            return x + 10

    return gen


def closure_coro():
    x = 1

    async def coro():
        await asyncio.sleep(0)
        return x

    return coro


def closure_delegating():
    x = 1

    def inner():
        yield x

    def outer():
        return (yield from inner())

    return outer


class ClosureSurvives(unittest.TestCase):
    def test_exhausted(self):
        gen = closure_gen()
        for _ in range(3):
            self.assertEqual(list(gen()), [1, 2])

    def test_throw_propagates(self):
        gen = closure_gen()
        for _ in range(3):
            it = gen()
            next(it)
            with self.assertRaises(ValueError):
                it.throw(ValueError())
        self.assertEqual(list(gen()), [1, 2])

    def test_throw_caught_then_returns(self):
        gen = closure_catching_gen()
        for _ in range(3):
            it = gen()
            next(it)
            with self.assertRaises(StopIteration) as caught:
                it.throw(ValueError())
            self.assertEqual(caught.exception.value, 11)

    def test_close(self):
        gen = closure_gen()
        for _ in range(3):
            it = gen()
            next(it)
            it.close()
        self.assertEqual(list(gen()), [1, 2])

    def test_throw_through_delegation(self):
        outer = closure_delegating()
        for _ in range(3):
            it = outer()
            next(it)
            with self.assertRaises(ValueError):
                it.throw(ValueError())
        self.assertEqual(list(outer()), [1])

    def test_coroutine_cancelled(self):
        coro = closure_coro()

        async def main():
            for _ in range(3):
                task = asyncio.ensure_future(coro())
                await asyncio.sleep(0)
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await task
            return await coro()

        self.assertEqual(asyncio.run(main()), 1)

    def test_coroutine_driven_by_hand(self):
        coro = closure_coro()
        for _ in range(3):
            c = coro()
            c.send(None)
            with self.assertRaises(StopIteration) as stopped:
                c.send(None)
            self.assertEqual(stopped.exception.value, 1)


if __name__ == "__main__":
    unittest.main()
