"""Generator methods and generator-made iterators, set up in place.

A loop's `gen.send(v)`, `coro.send(None)`, `gen.throw(...)` and
`gen.close()` load the bound method without leaving the core loop, and
`yield from obj` / `await obj` of an instance whose class's `__iter__` /
`__await__` is a generator function make that generator in place. The
results, exceptions and finalization must match the ordinary paths.
"""

import unittest


def averager():
    total = 0.0
    count = 0
    avg = None
    while True:
        value = yield avg
        total += value
        count += 1
        avg = total / count


class Pair:
    def __init__(self, a, b):
        self.a = a
        self.b = b

    def __iter__(self):
        yield self.a
        yield self.b


class Ready:
    def __await__(self):
        yield "tick"
        return 5


def flatten(items):
    for item in items:
        yield from item


async def waiter(n):
    total = 0
    for _ in range(n):
        total += await Ready()
    return total


def drive(coro):
    ticks = 0
    try:
        while True:
            assert coro.send(None) == "tick"
            ticks += 1
    except StopIteration as e:
        return e.value, ticks


class GeneratorMethodsInPlaceTest(unittest.TestCase):
    def test_send_loop(self):
        avg = averager()
        next(avg)
        last = None
        for i in range(3000):
            last = avg.send(i)
        self.assertEqual(last, sum(range(3000)) / 3000)

    def test_throw_and_close(self):
        def gen():
            try:
                while True:
                    yield 1
            except KeyError:
                yield "caught"

        for _ in range(500):
            g = gen()
            next(g)
            self.assertEqual(g.throw(KeyError), "caught")
            g.close()
            with self.assertRaises(StopIteration):
                next(g)

    def test_yield_from_instance(self):
        pairs = [Pair(i, -i) for i in range(1000)]
        self.assertEqual(sum(flatten(pairs)), 0)
        self.assertEqual(list(flatten([Pair(1, 2), Pair(3, 4)])), [1, 2, 3, 4])

    def test_instance_iter_attribute_ignored(self):
        p = Pair(1, 2)
        p.__iter__ = lambda: iter([9])
        self.assertEqual(list(flatten([p])), [1, 2])

    def test_await_instance(self):
        self.assertEqual(drive(waiter(2000)), (10000, 2000))


if __name__ == "__main__":
    unittest.main()
