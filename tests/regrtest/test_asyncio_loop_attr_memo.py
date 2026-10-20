"""asyncio's native loop fast paths read the loop's own attributes.

`call_soon`, `create_task` and `create_future` check a stock loop's
`_debug`, `_closed`, `_ready` and `_task_factory` natively, remembering
where an instance last held each name. Loops whose attributes sit at
other positions, debug mode switched on mid-run, and an instance-level
`get_debug` must all still be seen. A coroutine's bare `yield`
(`asyncio.sleep(0)`) reschedules its task for the next iteration.
"""

import asyncio
import types


class OddLoop(asyncio.SelectorEventLoop):
    """A loop whose instances set extra attributes first, so the names
    the fast paths read sit at other positions than a stock loop's."""

    def __init__(self):
        self.a1 = 1
        self.a2 = 2
        self.a3 = 3
        super().__init__()


async def ticker(n, log, tag):
    for i in range(n):
        log.append((tag, i))
        await asyncio.sleep(0)
    return tag


async def main(log):
    a = asyncio.ensure_future(ticker(3, log, "a"))
    b = asyncio.ensure_future(ticker(3, log, "b"))
    assert await asyncio.gather(a, b) == ["a", "b"]


def run_on(loop):
    log = []
    try:
        loop.run_until_complete(main(log))
    finally:
        loop.close()
    # Bare yields interleave the two tasks one step at a time.
    assert log == [("a", 0), ("b", 0), ("a", 1), ("b", 1), ("a", 2), ("b", 2)], log


# Stock loops and the odd layout, alternately, so each sees the other's
# remembered positions.
for _ in range(3):
    run_on(asyncio.new_event_loop())
    run_on(OddLoop())


# Debug mode switched on while running: handles made afterwards carry a
# source traceback (the Python `call_soon` path).
async def flip():
    loop = asyncio.get_running_loop()
    h1 = loop.call_soon(lambda: None)
    assert h1._source_traceback is None
    loop.set_debug(True)
    h2 = loop.call_soon(lambda: None)
    assert h2._source_traceback, h2._source_traceback
    loop.set_debug(False)
    h3 = loop.call_soon(lambda: None)
    assert h3._source_traceback is None
    await asyncio.sleep(0)


for make in (asyncio.new_event_loop, OddLoop):
    loop = make()
    try:
        loop.run_until_complete(flip())
    finally:
        loop.close()


# An instance-level `get_debug` overrides the stock one.
loop = OddLoop()
loop.get_debug = types.MethodType(lambda self: True, loop)
try:
    async def check():
        h = asyncio.get_running_loop().call_soon(lambda: None)
        assert h._source_traceback, "instance get_debug was ignored"

    loop.run_until_complete(check())
finally:
    del loop.get_debug
    loop.close()


# A task factory set on one loop doesn't leak into another.
made = []


def factory(loop, coro, **kw):
    made.append(coro)
    return asyncio.Task(coro, loop=loop, **kw)


l1, l2 = asyncio.new_event_loop(), OddLoop()
l1.set_task_factory(factory)
try:
    async def noop():
        return 7

    assert l1.run_until_complete(l1.create_task(noop())) == 7
    assert l2.run_until_complete(l2.create_task(noop())) == 7
    assert len(made) == 1, made
finally:
    l1.close()
    l2.close()


# A closed loop refuses callbacks.
loop = OddLoop()
loop.close()
try:
    loop.call_soon(print)
except RuntimeError as e:
    assert "closed" in str(e), e
else:
    raise AssertionError("call_soon on a closed loop")

print("ok")
