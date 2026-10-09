"""Generator and coroutine resumption from hot loops: send(), await of
fresh coroutines, and except-clause cleanup."""

import types
import unittest


async def leaf(n):
    return n


async def fib(n):
    if n < 2:
        return n
    return await fib(n - 1) + await fib(n - 2)


def run(coro):
    try:
        coro.send(None)
    except StopIteration as e:
        return e.value
    raise AssertionError("coroutine yielded")


class CoroutinePathsTest(unittest.TestCase):
    def test_send_and_await(self):
        for _ in range(50):
            self.assertEqual(run(fib(8)), 21)

    def test_generator_send_throw_close(self):
        def averager():
            total, count = 0.0, 0
            while True:
                try:
                    v = yield (total / count if count else None)
                except ValueError:
                    v = 0
                total += v
                count += 1

        g = averager()
        next(g)
        for i in range(1, 50):
            last = g.send(i)
        self.assertEqual(last, 25.0)
        self.assertEqual(g.throw(ValueError), 49 * 25.0 / 50)
        g.close()
        with self.assertRaises(StopIteration):
            g.send(1)

    def test_send_errors(self):
        def gen():
            yield 1

        for _ in range(3):
            g = gen()
            with self.assertRaises(TypeError):
                g.send(5)
            self.assertEqual(g.send(None), 1)
            with self.assertRaises(StopIteration):
                g.send(None)

    def test_awaited_twice(self):
        @types.coroutine
        def suspend():
            yield "suspended"

        async def inner():
            await suspend()
            return 1

        async def waiter(c):
            return await c

        c = inner()
        w1 = waiter(c)
        self.assertEqual(w1.send(None), "suspended")
        w2 = waiter(c)
        with self.assertRaises(RuntimeError):
            w2.send(None)
        w1.close()
        w2.close()
        c.close()

    def test_return_value_from_send(self):
        def gen():
            x = yield
            return x * 2

        for i in range(20):
            g = gen()
            next(g)
            with self.assertRaises(StopIteration) as cm:
                g.send(i)
            self.assertEqual(cm.exception.value, i * 2)

    def test_except_as_cleanup(self):
        def f(i):
            try:
                raise KeyError(i)
            except KeyError as e:
                k = e.args[0]
            try:
                e
            except NameError:
                return k
            return None

        for i in range(30):
            self.assertEqual(f(i), i)

    def test_del_local(self):
        def f():
            x = [1]
            del x
            try:
                x
            except UnboundLocalError:
                return "unbound"

        for _ in range(30):
            self.assertEqual(f(), "unbound")
        with self.assertRaises(UnboundLocalError):
            def g():
                del y
                y = 1
            g()


if __name__ == "__main__":
    unittest.main()
