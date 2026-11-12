"""`x.__class__` read from compiled code is `type(x)` for every receiver
kind, and an instance's class-level `__class__` override still wins."""
import asyncio


def gen():
    yield 1


async def coro():
    return 1


class Plain:
    pass


class Masquerade:
    @property
    def __class__(self):
        return int


def classes(objs):
    return [o.__class__ for o in objs]


objs = [1, 2.5, "s", b"b", None, True, [1], (1,), {1: 2}, {1}, gen, len, Plain(), Masquerade()]
expected = [type(o) for o in objs[:-1]] + [int]
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    g = gen()
    c = coro()
    assert classes(objs) == expected, classes(objs)
    assert classes([g, c]) == [type(g), type(c)]
    assert asyncio.isfuture(c) is False
    c.close()
print("ok")
