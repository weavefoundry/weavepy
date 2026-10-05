"""asyncio's native fast paths keep the Python-level semantics.

The native `Future`/`Task` drive coroutines and schedule callbacks
directly, `BaseEventLoop.call_soon` builds handles natively for a stock
loop, `Handle._run` is native, native tasks register in a native
registry (not `tasks.py`'s `WeakSet`), and an exact future or task with
nothing to log dies without running its `__del__`. These checks cover
the fallbacks (overridden or instance-patched `call_soon`, debug mode, a
closed loop), callback exceptions, cancellation bookkeeping,
`all_tasks`, eager tasks and task factories, and the "never retrieved"
and "destroyed but pending" reports.
"""

import asyncio
import gc
import sys


def run(coro, *, debug=False):
    loop = asyncio.new_event_loop()
    loop.set_debug(debug)
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


# --- call_soon: native path, overrides, and errors ----------------------

def check_call_soon():
    loop = asyncio.new_event_loop()
    got = []
    h = loop.call_soon(got.append, 1)
    assert type(h) is asyncio.Handle, type(h)
    assert h._callback == got.append and h._args == (1,)
    assert not h.cancelled() and h.get_context() is not None
    loop.call_soon(got.append, 2).cancel()
    loop.call_soon(got.append, 3)
    loop.call_soon(loop.stop)
    loop.run_forever()
    assert got == [1, 3], got

    # An exception in a callback goes to the loop's exception handler.
    seen = []
    loop.set_exception_handler(lambda lp, ctx: seen.append(ctx))

    def bad(x):
        raise ValueError(x)

    h = loop.call_soon(bad, 7)
    loop.call_soon(loop.stop)
    loop.run_forever()
    assert len(seen) == 1, seen
    assert isinstance(seen[0]["exception"], ValueError)
    assert seen[0]["handle"] is h
    assert seen[0]["message"].startswith("Exception in callback"), seen[0]

    # An instance-level override is honored by native schedulers.
    calls = []
    orig = loop.call_soon

    def patched(cb, *args, context=None):
        calls.append(cb)
        return orig(cb, *args, context=context)

    loop.call_soon = patched
    fut = loop.create_future()
    fut.add_done_callback(lambda f: None)
    fut.set_result(1)
    assert len(calls) == 1, calls
    del loop.call_soon
    loop.call_soon(loop.stop)
    loop.run_forever()

    loop.close()
    try:
        loop.call_soon(print)
    except RuntimeError as e:
        assert "closed" in str(e), e
    else:
        raise AssertionError("call_soon on a closed loop")
    try:
        asyncio.new_event_loop().call_soon()
    except TypeError:
        pass
    else:
        raise AssertionError("call_soon() without a callback")


check_call_soon()


class CountingLoop(asyncio.SelectorEventLoop):
    def __init__(self):
        super().__init__()
        self.scheduled = 0

    def call_soon(self, callback, *args, context=None):
        self.scheduled += 1
        return super().call_soon(callback, *args, context=context)


async def small_tree(depth):
    if depth == 0:
        await asyncio.sleep(0)
        return 1
    return sum(await asyncio.gather(small_tree(depth - 1), small_tree(depth - 1)))


loop = CountingLoop()
assert loop.run_until_complete(small_tree(3)) == 8
assert loop.scheduled > 8, loop.scheduled
loop.close()

# Debug mode takes the Python paths (source tracebacks on handles).
assert run(small_tree(2), debug=True) == 4


# --- Task semantics -------------------------------------------------------

async def task_semantics():
    loop = asyncio.get_running_loop()

    async def waiter(fut):
        return await fut

    fut = loop.create_future()
    t = asyncio.create_task(waiter(fut), name="w")
    await asyncio.sleep(0)
    assert t.get_name() == "w" and not t.done()
    assert t in asyncio.all_tasks()
    assert asyncio.current_task() is not t
    assert t.cancel("stop") and t.cancelling() == 1
    try:
        await t
    except asyncio.CancelledError as e:
        assert e.args == ("stop",), e.args
    else:
        raise AssertionError("expected CancelledError")
    assert t.cancelled() and t not in asyncio.all_tasks()
    assert t.uncancel() == 0

    async def raiser():
        await asyncio.sleep(0)
        raise KeyError("k")

    t = asyncio.create_task(raiser())
    try:
        await t
    except KeyError:
        tb = sys.exc_info()[2]
    else:
        raise AssertionError("expected KeyError")
    assert isinstance(t.exception(), KeyError)
    assert tb is not None

    # A Future subclass takes the generic (method-calling) path.
    class MyFuture(asyncio.Future):
        added = 0

        def add_done_callback(self, fn, *, context=None):
            MyFuture.added += 1
            super().add_done_callback(fn, context=context)

    mf = MyFuture(loop=loop)
    loop.call_soon(mf.set_result, 5)
    assert await mf == 5 and MyFuture.added == 1

    # Eager tasks run their first step synchronously.
    order = []

    async def eager_body():
        order.append("body")
        return 3

    loop.set_task_factory(asyncio.eager_task_factory)
    et = asyncio.create_task(eager_body())
    order.append("after")
    loop.set_task_factory(None)
    assert order == ["body", "after"], order
    assert et.done() and et.result() == 3

    # A custom task factory sees every task.
    made = []

    def factory(lp, coro, **kwargs):
        t = asyncio.Task(coro, loop=lp, **kwargs)
        made.append(t)
        return t

    loop.set_task_factory(factory)
    assert await asyncio.gather(eager_body(), eager_body()) == [3, 3]
    loop.set_task_factory(None)
    assert len(made) == 2, made

    # Awaiting a future from another loop fails the task.
    other = asyncio.new_event_loop()
    foreign = other.create_future()
    try:
        await asyncio.create_task(waiter(foreign))
    except RuntimeError as e:
        assert "attached to a different loop" in str(e), e
    else:
        raise AssertionError("expected RuntimeError")
    other.close()


run(task_semantics())


# --- Finalizer reports ----------------------------------------------------

def check_reports():
    loop = asyncio.new_event_loop()
    reports = []
    loop.set_exception_handler(lambda lp, ctx: reports.append(ctx["message"]))

    fut = loop.create_future()
    fut.set_exception(ValueError("lost"))
    del fut
    gc.collect()
    assert any("exception was never retrieved" in m for m in reports), reports

    # A retrieved exception and a plain result log nothing.
    reports.clear()
    fut = loop.create_future()
    fut.set_exception(ValueError("seen"))
    fut.exception()
    del fut
    fut = loop.create_future()
    fut.set_result(1)
    del fut
    gc.collect()
    assert reports == [], reports

    async def forever():
        await loop.create_future()

    t = loop.create_task(forever())
    loop.run_until_complete(asyncio.sleep(0))
    del t
    gc.collect()
    loop.run_until_complete(asyncio.sleep(0))
    gc.collect()
    assert any("Task was destroyed but it is pending" in m for m in reports), reports
    loop.close()


check_reports()
print("ok")
