"""Smoke test: async/await + asyncio.run + gather."""

import asyncio

async def double(x):
    await asyncio.sleep(0)
    return x * 2

async def main():
    results = await asyncio.gather(double(1), double(2), double(3))
    assert results == [2, 4, 6]

    a = await double(10)
    assert a == 20

asyncio.run(main())

# async for + async with via a tiny custom iterator
class Counter:
    def __init__(self, n):
        self.n = n
        self.i = 0

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self.i >= self.n:
            raise StopAsyncIteration
        self.i += 1
        return self.i


async def collect():
    out = []
    async for v in Counter(3):
        out.append(v)
    return out

assert asyncio.run(collect()) == [1, 2, 3]


class Trace:
    def __init__(self, label, log):
        self.label = label
        self.log = log

    async def __aenter__(self):
        self.log.append(f"enter:{self.label}")
        return self

    async def __aexit__(self, exc_type, exc, tb):
        self.log.append(f"exit:{self.label}")
        return False


async def with_runner():
    log = []
    async with Trace("a", log):
        log.append("body")
    return log

assert asyncio.run(with_runner()) == ["enter:a", "body", "exit:a"]
