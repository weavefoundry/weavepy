"""An asyncio task tree: gather, sleep(0), locks, and queues (after
pyperformance's `async_tree`)."""

import asyncio

WORK = 5


async def leaf(state):
    await asyncio.sleep(0)
    state[0] += 1
    return 1


async def node(depth, width, state, lock):
    if depth == 0:
        return await leaf(state)
    async with lock:
        state[1] += 1
    results = await asyncio.gather(*[node(depth - 1, width, state, lock) for _ in range(width)])
    return sum(results)


async def pipeline(n):
    q = asyncio.Queue()

    async def producer():
        for i in range(n):
            await q.put(i)
        await q.put(None)

    async def consumer():
        total = 0
        while True:
            item = await q.get()
            if item is None:
                return total
            total += item

    _, total = await asyncio.gather(producer(), consumer())
    return total


async def main(n):
    state = [0, 0]
    lock = asyncio.Lock()
    leaves = await node(n, 6, state, lock)
    total = await pipeline(2000 * n)
    return (leaves, state[0], state[1], total)


def bench(n):
    return asyncio.run(main(n))
