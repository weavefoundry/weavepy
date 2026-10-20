"""`gen.send(v)` and `coro.send(v)` as method calls: the method load
pushes the unbound `send` with the generator as its self, and the `CALL`
resumes the generator inline (see `core_gen_method` in the VM).

Each case runs warm (in loops long enough to compile) and checks the
general path's behaviors too: arity errors, sends into fresh, running
and finished generators, frames made visible, and the bound and unbound
forms of the method.
"""

import unittest

WARM = 3000


def averager():
    total = 0.0
    count = 0
    avg = None
    while True:
        value = yield avg
        total += value
        count += 1
        avg = total / count


def echo():
    value = None
    while True:
        value = yield value


class SendMethodTests(unittest.TestCase):
    def test_warm_sends(self):
        avg = averager()
        next(avg)
        last = None
        for i in range(WARM):
            last = avg.send(i)
        self.assertEqual(last, (WARM - 1) / 2)

    def test_arity(self):
        g = echo()
        next(g)
        for _ in range(WARM):
            g.send(1)
        with self.assertRaises(TypeError):
            g.send()
        with self.assertRaises(TypeError):
            g.send(1, 2)
        self.assertEqual(g.send(3), 3)

    def test_fresh_running_finished(self):
        for _ in range(WARM):
            g = echo()
            g.send(None)
        g = echo()
        with self.assertRaises(TypeError):
            g.send(1)
        self.assertIsNone(g.send(None))

        def selfsend():
            yield me.send(None)

        me = selfsend()
        with self.assertRaises(ValueError):
            next(me)

        def short():
            yield 1

        g = short()
        g.send(None)
        with self.assertRaises(StopIteration):
            g.send(None)
        with self.assertRaises(StopIteration):
            g.send(None)

    def test_visible_frame(self):
        g = echo()
        next(g)
        for i in range(WARM):
            self.assertEqual(g.send(i), i)
        self.assertEqual(g.gi_frame.f_locals["value"], WARM - 1)
        for i in range(10):
            self.assertEqual(g.send(i), i)
        self.assertFalse(g.gi_running)

    def test_bound_and_unbound_forms(self):
        g = echo()
        next(g)
        send = g.send
        for i in range(WARM):
            self.assertEqual(send(i), i)
        self.assertEqual(type(g).send(g, 7), 7)
        self.assertEqual(getattr(g, "send")(8), 8)
        with self.assertRaises(TypeError):
            type(g).send(1, 2)

    def test_coroutine_send(self):
        class Suspend:
            def __await__(self):
                return (yield "s")

        async def coro():
            total = 0
            while True:
                total += await Suspend()
                if total > 10 ** 9:
                    return total

        c = coro()
        self.assertEqual(c.send(None), "s")
        for i in range(WARM):
            self.assertEqual(c.send(i), "s")
        with self.assertRaises(StopIteration) as cm:
            c.send(10 ** 10)
        self.assertEqual(cm.exception.value, sum(range(WARM)) + 10 ** 10)
        c2 = coro()
        with self.assertRaises(TypeError):
            c2.send(1)
        c2.close()

    def test_exception_from_send(self):
        def raiser():
            while True:
                v = yield
                if v < 0:
                    raise KeyError(v)

        g = raiser()
        next(g)
        for i in range(WARM):
            g.send(i)
        with self.assertRaises(KeyError):
            g.send(-1)
        with self.assertRaises(StopIteration):
            g.send(1)


if __name__ == "__main__":
    unittest.main()
