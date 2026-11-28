"""The current decimal context across context switches.

Arithmetic finds the current context through a memo that every change of
the thread's current context, and every write to a context's mapping,
invalidates: `localcontext`, `setcontext`, `contextvars.Context.run`,
`copy_context`, asyncio tasks and threads all see their own context.
Run it past the compile threshold.
"""

import asyncio
import contextvars
import decimal
import threading

D = decimal.Decimal


def mul(n, a, b):
    r = None
    for _ in range(n):
        r = a * b
    return r


x, y = D("1.23456789"), D("9.87654321")
base = mul(2000, x, y)
assert base == D("12.19326311126352690")
with decimal.localcontext() as c:
    c.prec = 3
    assert mul(2000, x, y) == D("12.2")
    with decimal.localcontext(prec=5):
        assert mul(10, x, y) == D("12.193")
    assert mul(10, x, y) == D("12.2")
assert mul(2000, x, y) == base

ctx = decimal.Context(prec=4)
old = decimal.getcontext()
decimal.setcontext(ctx)
assert mul(10, x, y) == D("12.19")
decimal.setcontext(old)
assert mul(10, x, y) == base


def in_copy():
    decimal.setcontext(decimal.Context(prec=2))
    return mul(100, x, y)


assert contextvars.copy_context().run(in_copy) == D("12")
assert mul(10, x, y) == base

out = {}


def worker(prec, key):
    decimal.setcontext(decimal.Context(prec=prec))
    out[key] = mul(2000, x, y)


ts = [threading.Thread(target=worker, args=(p, p)) for p in (2, 6, 9)]
for t in ts:
    t.start()
for t in ts:
    t.join()
assert out == {2: D("12"), 6: D("12.1933"), 9: D("12.1932631")}, out
assert mul(10, x, y) == base


async def task(prec):
    decimal.setcontext(decimal.Context(prec=prec))
    await asyncio.sleep(0)
    return mul(500, x, y)


async def main():
    return await asyncio.gather(task(3), task(7))


assert asyncio.run(main()) == [D("12.2"), D("12.19326")]
assert mul(10, x, y) == base
print("ok")
