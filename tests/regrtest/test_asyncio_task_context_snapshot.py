"""A task's context is a copy of its creator's, kept across its steps: a
`ContextVar.set` in one step is seen by the next, by `get_context()` and
by the task's done callbacks, and not by its siblings or its creator, however
the task reaches the context (a set, `get_context()` before, during or after
a step, an eager start, a handle's `get_context()`, a loop whose
`call_soon` isn't native)."""

import asyncio
import contextvars

var = contextvars.ContextVar("var", default="unset")


async def setter(name, seen):
    seen.append((name, "start", var.get()))
    await asyncio.sleep(0)
    var.set(name)
    await asyncio.sleep(0)
    seen.append((name, "after", var.get()))
    fut = asyncio.get_running_loop().create_future()
    asyncio.get_running_loop().call_soon(fut.set_result, None)
    await fut
    seen.append((name, "wakeup", var.get()))
    return var.get()


async def reader(seen):
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    seen.append(("reader", var.get()))
    return var.get()


async def siblings():
    var.set("parent")
    seen = []
    results = await asyncio.gather(setter("a", seen), setter("b", seen), reader(seen))
    assert results == ["a", "b", "parent"], results
    assert ("a", "start", "parent") in seen, seen
    assert ("a", "after", "a") in seen and ("a", "wakeup", "a") in seen, seen
    assert ("b", "after", "b") in seen and ("b", "wakeup", "b") in seen, seen
    assert ("reader", "parent") in seen, seen
    assert var.get() == "parent"


async def get_context_during():
    task = asyncio.current_task()
    ctx = task.get_context()
    assert task.get_context() is ctx
    assert ctx is contextvars.copy_context() or ctx[var] == var.get()
    var.set("during")
    assert ctx[var] == "during", dict(ctx)
    await asyncio.sleep(0)
    assert task.get_context() is ctx
    assert var.get() == "during"
    return ctx


async def get_context_before_run():
    loop = asyncio.get_running_loop()
    task = loop.create_task(setter("early", []))
    ctx = task.get_context()  # before the first step
    assert ctx[var] == var.get()
    result = await task
    assert result == "early"
    assert task.get_context() is ctx
    assert ctx[var] == "early", dict(ctx)


async def set_then_get_context():
    async def body():
        var.set("body")
        await asyncio.sleep(0)
        return asyncio.current_task().get_context()

    task = asyncio.get_running_loop().create_task(body())
    ctx = await task
    assert ctx[var] == "body"
    assert task.get_context() is ctx


async def done_callback_context():
    seen = []

    async def body():
        var.set("in-task")
        await asyncio.sleep(0)

    task = asyncio.get_running_loop().create_task(body())
    task.add_done_callback(lambda t: seen.append(var.get()))
    await task
    await asyncio.sleep(0)
    # The callback runs in a copy of the context current when it was added.
    assert seen == [var.get()], seen


async def eager():
    loop = asyncio.get_running_loop()
    old = loop.get_task_factory()
    loop.set_task_factory(asyncio.eager_task_factory)
    try:
        async def body():
            var.set("eager")
            await asyncio.sleep(0)
            return var.get()

        task = loop.create_task(body())
        assert var.get() != "eager"
        assert await task == "eager"
        assert task.get_context()[var] == "eager"
    finally:
        loop.set_task_factory(old)


async def explicit_context():
    ctx = contextvars.copy_context()
    ctx.run(var.set, "explicit")

    async def body():
        await asyncio.sleep(0)
        return var.get()

    task = asyncio.get_running_loop().create_task(body(), context=ctx)
    assert task.get_context() is ctx
    assert await task == "explicit"


async def handle_context():
    loop = asyncio.get_running_loop()
    h = loop.call_soon(lambda: None)
    assert isinstance(h.get_context(), contextvars.Context)
    assert h.get_context() is h._context
    await asyncio.sleep(0)


async def reset_token():
    async def body():
        tok = var.set("x")
        await asyncio.sleep(0)
        var.reset(tok)
        await asyncio.sleep(0)
        return var.get()

    before = var.get()
    assert await asyncio.get_running_loop().create_task(body()) == before


async def main():
    var.set("main")
    await siblings()
    await asyncio.get_running_loop().create_task(get_context_during())
    await get_context_before_run()
    await set_then_get_context()
    await done_callback_context()
    await eager()
    await explicit_context()
    await handle_context()
    await reset_token()
    assert var.get() == "parent"


asyncio.run(main())


class SlowLoop(asyncio.SelectorEventLoop):
    """A loop whose `call_soon` is Python code."""

    def call_soon(self, callback, *args, context=None):
        return super().call_soon(callback, *args, context=context)


loop = SlowLoop()
try:
    loop.run_until_complete(main())
finally:
    loop.close()

print("ok")
