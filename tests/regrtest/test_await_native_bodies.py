"""Coroutine and `yield from` bodies once they're compiled: the native code
runs `GET_AWAITABLE`, `GET_YIELD_FROM_ITER`, `SEND` and `END_SEND` (see
`frame_jit` in the VM).

Each case warms a body until it compiles, then checks a behavior at the
boundary between the native code and the core loop: results, raises,
suspensions, `throw`/`close`, introspection of the delegate, and errors
for bad awaitables.
"""

import types
import unittest

WARM = 30000


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


async def fib(n):
    if n <= 1:
        return n
    return await fib(n - 1) + await fib(n - 2)


class Suspend:
    def __await__(self):
        value = yield "suspended"
        return value


async def waiter(n):
    total = 0
    for i in range(n):
        total += await Suspend() or i
    return total


async def leaf(x):
    if x < 0:
        raise ValueError(x)
    return x * 2


async def caller(x):
    return await leaf(x) + 1


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

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


def delegator(it):
    result = yield from it
    return result


def returner(n):
    yield n
    return n * 10


class AwaitNativeTests(unittest.TestCase):
    def test_fib(self):
        for _ in range(3):
            drive(fib(15))
        self.assertEqual(drive(fib(20)), 6765)

    def test_suspend_and_send(self):
        for _ in range(WARM // 100):
            drive(waiter(100))
        coro = waiter(3)
        self.assertEqual(coro.send(None), "suspended")
        self.assertIsNotNone(coro.cr_await)
        self.assertEqual(coro.send(10), "suspended")
        self.assertEqual(coro.send(None), "suspended")
        with self.assertRaises(StopIteration) as cm:
            coro.send(None)
        self.assertEqual(cm.exception.value, 10 + 1 + 2)
        self.assertIsNone(coro.cr_await)

    def test_raise_through_await(self):
        for i in range(WARM):
            drive(caller(i))
        try:
            drive(caller(-5))
        except ValueError as e:
            exc = e
        else:
            self.fail("no ValueError")
        self.assertEqual(exc.args, (-5,))
        names = []
        tb = exc.__traceback__
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-2:], ["caller", "leaf"])

    def test_throw_and_close(self):
        for _ in range(WARM // 100):
            drive(waiter(100))
        coro = waiter(5)
        coro.send(None)
        with self.assertRaises(KeyError):
            coro.throw(KeyError("x"))
        coro = waiter(5)
        coro.send(None)
        coro.close()
        self.assertIsNone(coro.cr_frame)

    def test_bad_awaitables(self):
        async def bad():
            return await 5

        async def twice():
            c = leaf(1)
            gen = c.__await__()
            next(gen, None)
            return await c

        for i in range(WARM):
            drive(caller(i))
        with self.assertRaises(TypeError):
            drive(bad())
        with self.assertRaises(RuntimeError):
            drive(twice())

    def test_types_coroutine(self):
        @types.coroutine
        def legacy():
            value = yield "legacy"
            return value

        async def uses(n):
            return await legacy() + n

        for i in range(WARM // 10):
            c = uses(i)
            c.send(None)
            try:
                c.send(1)
            except StopIteration as e:
                self.assertEqual(e.value, i + 1)

    def test_yield_from_tree(self):
        tree = build(0, 200)
        for _ in range(200):
            self.assertEqual(sum(tree), sum(range(200)))
        self.assertEqual(list(tree), list(range(200)))
        it = iter(tree)
        self.assertEqual(next(it), 0)
        self.assertIsNotNone(it.gi_yieldfrom)
        self.assertEqual(next(it), 1)
        it.close()
        self.assertIsNone(it.gi_frame)

    def test_yield_from_return_value(self):
        for i in range(WARM):
            list(delegator(returner(i)))
        gen = delegator(returner(4))
        self.assertEqual(next(gen), 4)
        with self.assertRaises(StopIteration) as cm:
            next(gen)
        self.assertEqual(cm.exception.value, 40)
        gen = delegator(iter([1, 2]))
        self.assertEqual(list(gen), [1, 2])


if __name__ == "__main__":
    unittest.main()
