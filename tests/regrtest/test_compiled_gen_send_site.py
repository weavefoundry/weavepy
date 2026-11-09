"""A compiled `x.send(v)` site keeps the generator kind's `send` once it
has loaded it: it goes on serving generators and coroutines correctly,
falls back for any other receiver with a `send` of its own, and still
raises as `send` does (a finished generator, the wrong argument count)."""


def averager():
    total = count = 0
    avg = None
    while True:
        value = yield avg
        total += value
        count += 1
        avg = total / count


class Coro:
    def __await__(self):
        return (yield "suspended")


async def coro_body():
    got = await Coro()
    return got * 2


class Sender:
    def __init__(self):
        self.seen = []

    def send(self, v):
        self.seen.append(v)
        return "sent %r" % (v,)


def drive(target, values):
    out = []
    for v in values:
        out.append(target.send(v))
    return out


for _ in range(3):
    g = averager()
    next(g)
    assert drive(g, range(1, 401))[-1] == 200.5

# The same site with a coroutine, then an object with its own send.
c = coro_body()
assert drive(c, [None]) == ["suspended"]
try:
    c.send(21)
except StopIteration as e:
    assert e.value == 42
else:
    raise AssertionError("the coroutine should have finished")
s = Sender()
assert drive(s, [1, 2]) == ["sent 1", "sent 2"]
assert s.seen == [1, 2]

# Back to generators at that site, then errors.
g = averager()
next(g)
assert drive(g, [10, 20, 30]) == [10.0, 15.0, 20.0]
g.close()
try:
    drive(g, [1])
except StopIteration:
    pass
else:
    raise AssertionError("a closed generator's send should raise")


def two_args(gen):
    return gen.send(1, 2)


h = averager()
next(h)
for _ in range(200):
    try:
        two_args(h)
    except TypeError as e:
        assert "send() takes exactly one argument" in str(e), e
    else:
        raise AssertionError("send(1, 2) should raise")


# A generator that sends to itself through the site raises as CPython does.
def selfish():
    yield me.send(None)


me = selfish()
try:
    next(me)
except ValueError as e:
    assert "already executing" in str(e)

print("ok")
