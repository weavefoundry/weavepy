"""RFC 0054 WS1 — the native `_asyncio` C accelerator.

CPython's `asyncio.futures`/`asyncio.tasks` swap their pure-Python
`Future`/`Task` for the `_asyncio` extension classes when the import
succeeds. This fixture proves the adoption hooks bind the native types
and that the accelerated state machines behave like CPython's: future
result/exception/cancel transitions, done-callback scheduling,
task cancellation bookkeeping (`cancelling()`/`uncancel()`), eager task
factories, and PEP 585 generic aliasing on both classes.
"""

import asyncio
import _asyncio


# ---------------------------------------------------------------------------
# Adoption: asyncio classes *are* the C-accelerator classes.
# ---------------------------------------------------------------------------

assert asyncio.Future is _asyncio.Future, asyncio.Future
assert asyncio.Task is _asyncio.Task, asyncio.Task
assert asyncio.isfuture(asyncio.get_event_loop_policy().new_event_loop().create_future())

# Both are generic in 3.13 (PEP 585).
assert asyncio.Future[int] is not None
assert asyncio.Task[str] is not None


# ---------------------------------------------------------------------------
# Future state machine: pending -> result / exception / cancelled.
# ---------------------------------------------------------------------------

async def future_machine():
    loop = asyncio.get_running_loop()

    f = loop.create_future()
    assert not f.done() and not f.cancelled()
    seen = []
    f.add_done_callback(lambda fut: seen.append(fut.result()))
    f.set_result(42)
    assert f.done() and f.result() == 42
    await asyncio.sleep(0)  # callbacks run via call_soon
    assert seen == [42], seen

    g = loop.create_future()
    g.set_exception(ValueError("boom"))
    try:
        g.result()
        raise AssertionError("expected ValueError")
    except ValueError as e:
        assert str(e) == "boom"
    assert type(g.exception()) is ValueError

    h = loop.create_future()
    assert h.cancel()
    assert h.cancelled() and h.done()
    try:
        h.result()
        raise AssertionError("expected CancelledError")
    except asyncio.CancelledError:
        pass


asyncio.run(future_machine())


# ---------------------------------------------------------------------------
# Task cancellation bookkeeping: cancelling() / uncancel().
# ---------------------------------------------------------------------------

async def task_cancel_bookkeeping():
    async def hang():
        try:
            await asyncio.sleep(60)
        except asyncio.CancelledError:
            raise

    t = asyncio.ensure_future(hang())
    assert isinstance(t, _asyncio.Task)
    await asyncio.sleep(0)
    assert t.cancelling() == 0
    t.cancel("stop-1")
    assert t.cancelling() == 1
    assert t.uncancel() == 0
    t.cancel("stop-2")
    try:
        await t
        raise AssertionError("expected CancelledError")
    except asyncio.CancelledError:
        pass
    assert t.cancelled()


asyncio.run(task_cancel_bookkeeping())


# ---------------------------------------------------------------------------
# Eager task factory: the coroutine starts synchronously at create time.
# ---------------------------------------------------------------------------

async def eager_start():
    loop = asyncio.get_running_loop()
    loop.set_task_factory(asyncio.eager_task_factory)
    steps = []

    async def quick():
        steps.append("ran")
        return "done"

    t = asyncio.ensure_future(quick())
    # An eager task with no awaits finishes before control returns.
    assert steps == ["ran"], steps
    assert t.done()
    assert await t == "done"


asyncio.run(eager_start())


# ---------------------------------------------------------------------------
# Module-level accelerator helpers.
# ---------------------------------------------------------------------------

async def running_loop_visible():
    assert _asyncio._get_running_loop() is asyncio.get_running_loop()
    t = asyncio.current_task()
    assert isinstance(t, _asyncio.Task)
    assert t in asyncio.all_tasks()


asyncio.run(running_loop_visible())
assert _asyncio._get_running_loop() is None

print("RFC 0054 native _asyncio fixture ok")
