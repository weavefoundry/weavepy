"""Asyncio handles, built natively by `call_soon` or by `Handle(...)`, share
one slot layout and keep CPython's attributes in compiled code: the
callback, arguments and loop, `_cancelled` before and after `cancel()`,
a `TimerHandle`'s own slots, and `__class__` of the built-in values
`asyncio.isfuture` tests."""
import asyncio


def noop(*args):
    pass


def flags(handles):
    return [h._cancelled for h in handles]


async def main():
    loop = asyncio.get_running_loop()
    natives = [loop.call_soon(noop, i) for i in range(3000)]
    built = [asyncio.Handle(noop, (i,), loop) for i in range(3000)]
    for hs in (natives, built):
        assert not any(flags(hs))
        assert all(h._callback is noop and h._loop is loop for h in hs)
        assert [h._args for h in hs[:3]] == [(0,), (1,), (2,)]
        for h in hs[::2]:
            h.cancel()
        assert flags(hs)[:4] == [True, False, True, False]
        assert hs[0].cancelled() and "cancelled" in repr(hs[0])
    t = loop.call_later(60, noop)
    assert type(t).__name__ == "TimerHandle" and t._cancelled is False
    t.cancel()
    assert t._cancelled and t.when() > loop.time()

    async def coro():
        return 1

    for _ in range(3000):
        c = coro()
        assert c.__class__.__name__ == "coroutine" and not asyncio.isfuture(c)
        c.close()
    assert (1).__class__ is int and [].__class__ is list and "".__class__ is str
    assert asyncio.isfuture(loop.create_future())


asyncio.run(main())
print("ok")
