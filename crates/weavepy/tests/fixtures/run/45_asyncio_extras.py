# weavepy-skip: windows
#
# asyncio is built on the Unix-only `select`/`selectors` backend right now
# (no IOCP / winsock `select(2)` adapter), so the event loop can't start on
# Windows (`ModuleNotFoundError: No module named '_overlapped'`). Unix-only
# until that backend lands.
#
# Extra asyncio primitives layered on top of the core event loop:
# `to_thread`, `as_completed`, `shield`, `wrap_future`, and the
# `BoundedSemaphore` / `Condition` / `LifoQueue` / `PriorityQueue`
# coordination types. Each exercise is small but verifies that the
# Python-level implementations slot cleanly into the cooperative loop.

import asyncio


def square(x):
    return x * x


async def main():
    # to_thread runs a plain function and lets us await its result.
    print("to_thread:", await asyncio.to_thread(square, 7))

    # as_completed yields tasks in completion order.
    async def echo(v):
        return v

    results = []
    for fut in asyncio.as_completed([echo(i) for i in range(3)]):
        results.append(await fut)
    print("as_completed:", sorted(results))

    # BoundedSemaphore — release past initial count raises.
    bs = asyncio.BoundedSemaphore(1)
    await bs.acquire()
    bs.release()
    try:
        bs.release()
    except ValueError as e:
        print("bounded:", e)

    # Condition + predicate
    cv = asyncio.Condition()
    state = [0]

    async def waker():
        async with cv:
            state[0] = 1
            cv.notify_all()

    async def waiter():
        async with cv:
            await cv.wait_for(lambda: state[0] == 1)
            return "woken"

    t = asyncio.create_task(waiter())
    await asyncio.sleep(0)
    await waker()
    print("condition:", await t)

    # LifoQueue ordering
    lq = asyncio.LifoQueue()
    for n in range(3):
        await lq.put(n)
    print("lifo:", [await lq.get() for _ in range(3)])

    # PriorityQueue ordering
    pq = asyncio.PriorityQueue()
    for n in [3, 1, 4, 1, 5, 9, 2, 6]:
        await pq.put(n)
    print("prio:", [await pq.get() for _ in range(8)])

    # isfuture / iscoroutine / iscoroutinefunction now look at co_flags.
    async def coro():
        return 1

    def sync():
        return 1

    f = asyncio.get_event_loop().create_future()
    print(
        "introspect:",
        asyncio.isfuture(f),
        asyncio.iscoroutinefunction(coro),
        asyncio.iscoroutinefunction(sync),
        asyncio.iscoroutine(coro()),
    )


asyncio.run(main())
