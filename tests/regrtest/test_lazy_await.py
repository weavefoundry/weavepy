"""`await f(...)` of a coroutine function runs its body before the coroutine
exists, making it only when something needs it.

A compiled caller's `await f(...)` runs the callee's body as a direct
call, and a body that returns without suspending never makes its
coroutine. Anything that can see the coroutine must still find it, as
it was when the call made it: a suspension (and `cr_await`, `cr_frame`,
`throw()` and `close()` while suspended), a raise and its traceback,
`sys._getframe()` and the frame's generator, `locals()`, a trace or
profile hook installed partway through a body, the collector's
referrers, a renamed function's `cr_name`, PEP 479's conversion, the
recursion limit, and finalizers. Each case runs hot first, so the
lazily run body is the one observed.
"""

import gc
import inspect
import os
import subprocess
import sys
import traceback
import types
import unittest

# The bodies compile after a few calls (WeavePy's frame JIT), so each case
# reaches the lazily run body quickly.
if sys.implementation.name == "weavepy" and "WEAVEPY_FRAME_JIT_HOT" not in os.environ:
    env = dict(os.environ, WEAVEPY_FRAME_JIT_HOT="50")
    sys.exit(subprocess.run([sys.executable, __file__, *sys.argv[1:]], env=env).returncode)

WARM = 3000


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


@types.coroutine
def suspend(value=None):
    return (yield value)


class Awaitable:
    def __init__(self, value):
        self.value = value

    def __await__(self):
        yield "aw"
        return self.value


# -- Plain returns -------------------------------------------------------


async def leaf(a, b=10):
    return a + b


class Box:
    def __init__(self, k):
        self.k = k

    async def get(self, a):
        return self.k + a


async def fib(n):
    if n <= 1:
        return n
    return await fib(n - 1) + await fib(n - 2)


async def returns_in_loop(n):
    for i in range(n):
        if i == 2:
            return i
    return -1


async def returns_in_with(cm):
    with cm:
        return 7


class CM:
    def __init__(self):
        self.log = []

    def __enter__(self):
        self.log.append("enter")

    def __exit__(self, *a):
        self.log.append("exit")


# -- Observers -----------------------------------------------------------


async def frame_probe(probe):
    if probe:
        f = sys._getframe()
        g = f.f_generator
        return (
            f.f_code.co_name,
            f.f_back.f_code.co_name,
            type(g).__name__,
            g.cr_frame is f,
            g.cr_running,
            inspect.getcoroutinestate(g),
            sorted(f.f_locals),
        )
    return None


async def frame_outer(probe):
    return await frame_probe(probe)


async def locals_probe(a, probe):
    b = a + 1
    if probe:
        return sorted(locals().items())
    return b


async def suspending(probe):
    if probe:
        got = await suspend("s")
        return ("resumed", got)
    return "plain"


async def suspend_outer(probe):
    r = await suspending(probe)
    return ("outer", r)


async def raising(probe):
    if probe:
        raise ValueError("boom")
    return 1


async def raising_outer(probe):
    return await raising(probe)


async def catching(probe):
    try:
        if probe:
            raise KeyError("k")
    except KeyError as e:
        return ("caught", repr(e))
    return "none"


async def stop_iter(probe):
    if probe:
        raise StopIteration(5)
    return 0


async def call_hook(hook, probe):
    if probe:
        hook()
        x = 1
        x += 1
        return x
    return 0


def type_names(objs):
    return [type(o).__name__ for o in objs]


async def referrer_probe(obj, probe):
    if probe:
        return type_names(gc.get_referrers(obj))
    return None


async def await_awaitable(probe):
    if probe:
        return await Awaitable(3)
    return 0


async def deep(n):
    if n == 0:
        return 0
    return await deep(n - 1) + 1


class Fin:
    log = []

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        Fin.log.append(self.tag)


async def with_finalizer(tag):
    f = Fin(tag)
    return tag


async def drops_finalizer(holder, tag):
    holder.clear()
    return tag


async def renamed(probe):
    if probe:
        return sys._getframe().f_generator.__name__
    return None


async def nameprobe(probe):
    if probe:
        g = sys._getframe().f_generator
        return (g.__name__, g.__qualname__)
    return None


# -- Callers (each runs hot) ---------------------------------------------


async def run_many(coro_fn):
    out = None
    for i in range(WARM):
        out = await coro_fn(i == WARM - 1)
    return out


async def run_many_with(coro_fn, arg):
    out = None
    for i in range(WARM):
        out = await coro_fn(arg, i == WARM - 1)
    return out


async def warm_calls():
    t = 0
    box = Box(5)
    for i in range(WARM):
        t += await leaf(i)
        t += await leaf(i, 1)
        t += await box.get(i)
    return t


class LazyAwaitTest(unittest.TestCase):
    def test_values(self):
        expect = sum(i + 10 + i + 1 + 5 + i for i in range(WARM))
        self.assertEqual(drive(warm_calls()), expect)
        self.assertEqual(drive(fib(18)), 2584)

    def test_returns_in_blocks(self):
        async def run():
            cm = CM()
            out = []
            for _ in range(WARM):
                out.append(await returns_in_loop(5))
                out.append(await returns_in_with(cm))
            return out[-2:], cm.log[-2:], len(cm.log)

        self.assertEqual(drive(run()), ([2, 7], ["enter", "exit"], 2 * WARM))

    def test_frame(self):
        r = drive(run_many(frame_probe))
        self.assertEqual(
            r,
            ("frame_probe", "run_many", "coroutine", True, True, "CORO_RUNNING", ["f", "g", "probe"]),
        )

        async def run():
            out = None
            for i in range(WARM):
                out = await frame_outer(i == WARM - 1)
            return out

        r = drive(run())
        self.assertEqual(r[:2], ("frame_probe", "frame_outer"))

    def test_locals(self):
        self.assertEqual(drive(run_many_with(locals_probe, 4)), [("a", 4), ("b", 5), ("probe", True)])

    def test_suspension(self):
        async def run():
            out = None
            for i in range(WARM):
                out = await suspend_outer(i == WARM - 1)
            return out

        c = run()
        self.assertEqual(c.send(None), "s")
        # Suspended in the innermost body: the chain of coroutines is real.
        inner = c.cr_await
        self.assertEqual(type(inner).__name__, "coroutine")
        self.assertEqual(inner.cr_code.co_name, "suspend_outer")
        self.assertEqual(inner.cr_await.cr_code.co_name, "suspending")
        self.assertEqual(inspect.getcoroutinestate(inner), "CORO_SUSPENDED")
        self.assertEqual(inner.cr_await.cr_frame.f_code.co_name, "suspending")
        with self.assertRaises(StopIteration) as cm:
            c.send("v")
        self.assertEqual(cm.exception.value, ("outer", ("resumed", "v")))

    def test_throw_and_close(self):
        async def run():
            out = None
            for i in range(WARM):
                try:
                    out = await suspend_outer(i == WARM - 1)
                except KeyError as e:
                    return ("thrown", repr(e))
            return out

        c = run()
        self.assertEqual(c.send(None), "s")
        with self.assertRaises(StopIteration) as cm:
            c.throw(KeyError("t"))
        self.assertEqual(cm.exception.value, ("thrown", "KeyError('t')"))
        c = run()
        self.assertEqual(c.send(None), "s")
        inner = c.cr_await
        c.close()
        self.assertEqual(inspect.getcoroutinestate(inner), "CORO_CLOSED")

    def test_traceback(self):
        async def run():
            for i in range(WARM):
                await raising_outer(i == WARM - 1)

        try:
            drive(run())
        except ValueError as e:
            tb = traceback.extract_tb(e.__traceback__)
            names = [f.name for f in tb]
            self.assertEqual(names[-3:], ["run", "raising_outer", "raising"])
            self.assertEqual(tb[-2].line, "return await raising(probe)")
        else:
            self.fail("no raise")

    def test_catch_and_pep479(self):
        self.assertEqual(drive(run_many(catching)), ("caught", "KeyError('k')"))
        with self.assertRaises(RuntimeError) as cm:
            drive(run_many(stop_iter))
        self.assertIsInstance(cm.exception.__cause__, StopIteration)

    def test_trace_installed_partway(self):
        events = []

        def tracer(frame, event, arg):
            if frame.f_code.co_name == "call_hook":
                events.append((event, frame.f_lineno - call_hook.__code__.co_firstlineno))
            return tracer

        def hook():
            sys.settrace(tracer)
            sys._getframe().f_back.f_trace = tracer

        try:
            r = drive(run_many_with(call_hook, hook))
        finally:
            sys.settrace(None)
        self.assertEqual(r, 2)
        self.assertEqual(events[:3], [("line", 3), ("line", 4), ("line", 5)])
        self.assertIn(("return", 5), events)

    def test_profile_installed_partway(self):
        events = []

        def prof(frame, event, arg):
            if frame.f_code.co_name in ("call_hook", "run_many_with"):
                events.append((event, frame.f_code.co_name))

        def hook():
            sys.setprofile(prof)

        try:
            drive(run_many_with(call_hook, hook))
        finally:
            sys.setprofile(None)
        self.assertEqual(events[:2], [("return", "call_hook"), ("return", "run_many_with")])

    def test_referrers(self):
        obj = object()
        r = drive(run_many_with(referrer_probe, obj))
        self.assertIn("coroutine", r)

    def test_awaitables(self):
        self.assertEqual(drive(run_many(await_awaitable)), 3)

    def test_renamed(self):
        async def run():
            out = None
            for i in range(WARM):
                if i == WARM - 2:
                    renamed.__name__ = "other"
                    renamed.__qualname__ = "Other.other"
                out = await renamed(i == WARM - 1)
            return out

        self.assertEqual(drive(run()), "other")
        self.assertEqual(drive(run_many(nameprobe)), ("nameprobe", "nameprobe"))

    def test_recursion_limit(self):
        async def run(n):
            return await deep(n)

        old = sys.getrecursionlimit()
        try:
            for _ in range(WARM // 100):
                drive(run(50))
            sys.setrecursionlimit(200)
            with self.assertRaises(RecursionError):
                drive(run(1000))
        finally:
            sys.setrecursionlimit(old)

    def test_finalizers(self):
        async def run():
            out = []
            for i in range(WARM):
                Fin.log.clear()
                tag = await with_finalizer(i)
                out.append(Fin.log == [i] and tag == i)
                Fin.log.clear()
                tag = await drops_finalizer([Fin(-i)], i)
                out.append(Fin.log == [-i] and tag == i)
            return all(out)

        self.assertTrue(drive(run()))

    def test_never_awaited_unchanged(self):
        import warnings

        async def run():
            for i in range(WARM):
                await leaf(i)
            c = leaf(1)
            del c

        with warnings.catch_warnings(record=True) as w:
            warnings.simplefilter("always")
            drive(run())
            gc.collect()
        self.assertTrue(any("never awaited" in str(x.message) for x in w))

    def test_asyncio(self):
        import asyncio

        async def child(i):
            if i % 7 == 0:
                await asyncio.sleep(0)
            return i

        async def main():
            t = 0
            for i in range(WARM):
                t += await child(i)
            task = asyncio.ensure_future(child(7))
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                t += 1
            return t

        self.assertEqual(asyncio.run(main()), sum(range(WARM)) + 1)


if __name__ == "__main__":
    unittest.main()
