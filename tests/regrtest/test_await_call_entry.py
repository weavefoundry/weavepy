"""`await f(...)` of a coroutine function starts the coroutine at once.

The call that makes the coroutine also starts it, as the `SEND` after
`GET_AWAITABLE` would. Everything an await shows must stay as before:
results, exceptions and their tracebacks, PEP 479's conversion, the
coroutine's frame and state while it runs, suspension through the await,
recursion limits, and trace events.
"""

import sys
import types
import unittest


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


@types.coroutine
def suspend(value=None):
    return (yield value)


async def fib(n):
    if n <= 1:
        return n
    return await fib(n - 1) + await fib(n - 2)


async def fail(exc):
    raise exc


async def await_fail(exc):
    return await fail(exc)


class Box:
    def __init__(self, v):
        self.v = v

    async def get(self):
        return self.v

    async def total(self):
        return await self.get() + await self.get()


class AwaitCallEntryTest(unittest.TestCase):
    def test_results(self):
        self.assertEqual(drive(fib(15)), 610)
        self.assertEqual(drive(Box(21).total()), 42)

    def test_exception_and_traceback(self):
        try:
            drive(await_fail(KeyError("k")))
        except KeyError as e:
            tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-3:], ["drive", "await_fail", "fail"])

    def test_stop_iteration_becomes_runtime_error(self):
        with self.assertRaises(RuntimeError) as cm:
            drive(await_fail(StopIteration(1)))
        self.assertIn("coroutine raised StopIteration", str(cm.exception))
        self.assertIsInstance(cm.exception.__cause__, StopIteration)

    def test_caught_in_caller(self):
        async def caller():
            try:
                await fail(ValueError("v"))
            except ValueError as e:
                return e.args
        self.assertEqual(drive(caller()), ("v",))

    def test_frame_and_state_while_running(self):
        seen = {}

        async def inner():
            frame = sys._getframe()
            coro = frame.f_generator
            seen["running"] = coro.cr_running
            seen["frame"] = coro.cr_frame is frame
            seen["name"] = coro.__qualname__
            seen["back"] = frame.f_back.f_code.co_name
            seen["line"] = frame.f_back.f_lineno
            return 7

        async def outer():
            return await inner()

        self.assertEqual(drive(outer()), 7)
        self.assertTrue(seen["running"])
        self.assertTrue(seen["frame"])
        self.assertTrue(seen["name"].endswith("inner"))
        self.assertEqual(seen["back"], "outer")
        self.assertEqual(seen["line"], outer.__code__.co_firstlineno + 1)

    def test_suspension_through_the_await(self):
        async def inner():
            got = await suspend("first")
            got2 = await suspend("second")
            return got + got2

        async def outer():
            return await inner() * 2

        coro = outer()
        self.assertEqual(coro.send(None), "first")
        self.assertEqual(coro.cr_await.cr_code.co_name, "inner")
        self.assertFalse(coro.cr_await.cr_running)
        self.assertEqual(coro.send(3), "second")
        with self.assertRaises(StopIteration) as cm:
            coro.send(4)
        self.assertEqual(cm.exception.value, 14)

    def test_throw_and_close_while_suspended(self):
        log = []

        async def inner():
            try:
                await suspend()
            except KeyError:
                log.append("caught")
                return "recovered"
            finally:
                log.append("finally")

        async def outer():
            return await inner()

        coro = outer()
        coro.send(None)
        with self.assertRaises(StopIteration) as cm:
            coro.throw(KeyError())
        self.assertEqual(cm.exception.value, "recovered")
        self.assertEqual(log, ["caught", "finally"])
        coro = outer()
        coro.send(None)
        coro.close()
        self.assertEqual(log, ["caught", "finally", "finally"])

    def test_gc_tracking(self):
        import gc
        import weakref

        class Node:
            pass

        seen = {}

        async def inner(node):
            coro = sys._getframe().f_generator
            seen["tracked"] = gc.is_tracked(coro)
            # A cycle through the coroutine, live while it is suspended.
            node.coro = coro
            await suspend()

        async def outer(node):
            await inner(node)

        node = Node()
        coro = outer(node)
        coro.send(None)
        self.assertTrue(seen["tracked"])
        self.assertTrue(gc.is_tracked(node.coro))
        ref = weakref.ref(node)
        del node, coro
        gc.collect()
        self.assertIsNone(ref())

    def test_recursion_limit(self):
        async def deep(n):
            return await deep(n + 1)

        with self.assertRaises(RecursionError):
            drive(deep(0))

    def test_renamed_function(self):
        async def named():
            return sys._getframe().f_generator.__name__

        named.__name__ = "renamed"

        async def outer():
            return await named()

        self.assertEqual(drive(outer()), "renamed")

    def test_trace_events(self):
        events = []

        async def inner():
            return 1

        async def outer():
            return await inner()

        def tracer(frame, event, arg):
            if frame.f_code in (inner.__code__, outer.__code__):
                events.append((frame.f_code.co_name, event))
            return tracer

        sys.settrace(tracer)
        try:
            drive(outer())
        finally:
            sys.settrace(None)
        self.assertEqual(
            events,
            [
                ("outer", "call"),
                ("outer", "line"),
                ("inner", "call"),
                ("inner", "line"),
                ("inner", "return"),
                ("outer", "exception"),
                ("outer", "return"),
            ],
        )


if __name__ == "__main__":
    unittest.main()
