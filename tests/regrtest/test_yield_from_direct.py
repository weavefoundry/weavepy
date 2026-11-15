"""A loop over a generator that delegates with `yield from` gets what the
delegation gives, level by level: the delegate's items, its return value
as the expression's value, items after the delegation, nested and
recursive delegation, a delegate that raises or is closed, sends and
throws reaching the delegate, and a delegate reaching back for its
delegator while it runs."""


def inner(n):
    for i in range(n):
        yield i
    return n * 10


def outer(n):
    yield "start"
    got = yield from inner(n)
    yield ("returned", got)
    got2 = yield from inner(2)
    yield ("again", got2)


def deep(level, n):
    if level == 0:
        r = yield from inner(n)
        return r + 1
    r = yield from deep(level - 1, n)
    yield ("level", level, r)
    return r + 1


def walk(d):
    if d == 0:
        yield 1
    else:
        yield from walk(d - 1)
        yield d
        yield from walk(d - 1)


def collect(g):
    out = []
    for x in g:
        out.append(x)
    return out


def total(g):
    t = 0
    for x in g:
        t += x
    return t


def failing(n):
    yield 1
    yield 2
    raise KeyError(n)


def wraps_failing(n):
    try:
        yield from failing(n)
    except KeyError as e:
        yield ("caught", e.args[0])
    yield "after"


def echo():
    got = []
    try:
        while True:
            got.append((yield len(got)))
    except GeneratorExit:
        got.append("closed")
        raise
    finally:
        LOG.append(got)


def wraps_echo():
    yield from echo()


LOG = []
HOLDER = []


def reaches_back():
    yield 1
    try:
        next(HOLDER[0])
    except ValueError as e:
        yield str(e)
    yield 3


def delegator():
    yield from reaches_back()


N = 300
for _ in range(20):
    assert collect(outer(3)) == ["start", 0, 1, 2, ("returned", 30), 0, 1, ("again", 20)]
    assert collect(deep(3, 2)) == [0, 1, ("level", 1, 21), ("level", 2, 22), ("level", 3, 23)]
    assert total(walk(4)) == 16 + 8 * 1 + 4 * 2 + 2 * 3 + 4
    assert collect(wraps_failing(7)) == [1, 2, ("caught", 7), "after"]

for _ in range(3):
    assert total(walk(8)) == sum(walk(8))
    assert collect(outer(N))[-4:] == [("returned", N * 10), 0, 1, ("again", 20)]

# Sends and a throw reach the delegate; closing the delegator closes it.
for _ in range(200):
    LOG.clear()
    g = wraps_echo()
    assert next(g) == 0
    for x in g:
        assert x == 1
        break
    assert g.send("a") == 2
    try:
        g.throw(GeneratorExit)
    except GeneratorExit:
        pass
    assert LOG == [[None, "a", "closed"]], LOG

# A delegate that resumes its delegator finds it running.
for _ in range(200):
    HOLDER[:] = [delegator()]
    out = collect(HOLDER[0])
    assert out == [1, "generator already executing", 3], out


# A delegate dropped mid-loop runs its cleanup; the delegator's later
# items are never produced.
def guarded(log):
    try:
        yield 1
        yield 2
    finally:
        log.append("inner done")


def wraps_guarded(log):
    try:
        yield from guarded(log)
        yield "not reached"
    finally:
        log.append("outer done")


import gc

for _ in range(100):
    log = []
    for x in wraps_guarded(log):
        break
    gc.collect()
    assert log == ["inner done", "outer done"], log
