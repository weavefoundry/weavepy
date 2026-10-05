"""Native coroutines awaiting each other without an event loop (as in
pyperformance's `coroutines`), plus a generator-based pipeline driven by
`send`."""

WORK = 25


class Awaitable:
    def __await__(self):
        yield None


async def fibonacci(n):
    if n <= 1:
        return n
    return await fibonacci(n - 1) + await fibonacci(n - 2)


async def with_yields(n):
    total = 0
    for i in range(n):
        await Awaitable()
        total += i
    return total


def averager():
    total = 0.0
    count = 0
    avg = None
    while True:
        value = yield avg
        total += value
        count += 1
        avg = total / count


def drive(coro):
    try:
        while True:
            coro.send(None)
    except StopIteration as e:
        return e.value


def bench(n):
    r = drive(fibonacci(n))
    r += drive(with_yields(n * 4000))
    avg = averager()
    next(avg)
    last = 0.0
    for i in range(n * 8000):
        last = avg.send(i)
    return (r, last)
