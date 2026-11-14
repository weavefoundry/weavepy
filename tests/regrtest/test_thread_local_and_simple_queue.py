"""`threading.local` and `queue.SimpleQueue` through their native paths:
each thread's own namespace on every read, store and delete in hot code,
`__init__` once a thread, values released when a thread ends or a local
goes away, and a queue's items in order through `get`, `get_nowait`,
timeouts and waiting getters."""

import gc
import queue
import threading
import time
import weakref

N = 4000


# -- threading.local ----------------------------------------------------------

loc = threading.local()


def reads(n):
    acc = 0
    for _ in range(n):
        acc += loc.x
    return acc


def stores(n):
    for i in range(n):
        loc.x = i
    return loc.x


def churn(n):
    hits = 0
    for i in range(n):
        loc.tmp = i
        assert loc.tmp == i and loc.__dict__["tmp"] == i
        del loc.tmp
        try:
            loc.tmp
        except AttributeError:
            hits += 1
        try:
            del loc.tmp
        except AttributeError:
            hits += 1
    return hits


loc.x = 2
for _ in range(3):
    assert reads(N) == 2 * N
    assert stores(N) == N - 1
    loc.x = 2
    assert churn(N) == 2 * N
assert loc.__dict__ == {"x": 2}
assert loc.__dict__ is loc.__dict__
loc.__dict__["y"] = 5
assert loc.y == 5 and vars(loc) == {"x": 2, "y": 5}
for name in ("__dict__",):
    try:
        setattr(loc, name, {})
    except AttributeError:
        pass
    else:
        raise AssertionError("__dict__ was assigned")
    try:
        delattr(loc, name)
    except AttributeError:
        pass
    else:
        raise AssertionError("__dict__ was deleted")
assert getattr(loc, "missing", "default") == "default" and not hasattr(loc, "missing")


# Every thread has its own namespace, in hot code too.
def worker(k, out):
    assert not hasattr(loc, "x")
    loc.x = k
    out[k] = (reads(N), stores(N), churn(200))
    loc.x = k
    out[k] += (loc.x, sorted(loc.__dict__))


out = {}
threads = [threading.Thread(target=worker, args=(k, out)) for k in range(1, 5)]
for t in threads:
    t.start()
for t in threads:
    t.join()
assert out == {k: (k * N, N - 1, 400, k, ["x"]) for k in range(1, 5)}, out
assert loc.x == 2 and reads(N) == 2 * N


# A subclass: `__init__` runs once a thread, with the constructor's
# arguments, and a thread's own entry shadows a class attribute.
inits = []


class Settings(threading.local):
    mode = "class"
    limit = 10

    def __init__(self, base, scale=1):
        inits.append(threading.get_ident())
        self.base = base
        self.scale = scale

    def scaled(self):
        return self.base * self.scale

    @property
    def doubled(self):
        return self.base * 2


settings = Settings(3, scale=4)
assert inits == [threading.get_ident()]
assert (settings.base, settings.scale, settings.mode, settings.limit) == (3, 4, "class", 10)
assert settings.scaled() == 12 and settings.doubled == 6
settings.mode = "mine"
assert settings.mode == "mine" and Settings.mode == "class"
del settings.mode
assert settings.mode == "class"


def subclass_worker(k, out):
    first = (settings.base, settings.scale, settings.mode)
    settings.base = k
    settings.mode = "thread"
    total = 0
    for _ in range(N):
        total += settings.scaled() + settings.limit
    out[k] = first + (total, settings.doubled, settings.mode)


out = {}
threads = [threading.Thread(target=subclass_worker, args=(k, out)) for k in range(1, 4)]
for t in threads:
    t.start()
for t in threads:
    t.join()
assert out == {k: (3, 4, "class", (4 * k + 10) * N, 2 * k, "thread") for k in range(1, 4)}, out
assert len(inits) == 4 and len(set(inits)) == 4
assert (settings.base, settings.mode) == (3, "class")

try:
    threading.local(1)
except TypeError:
    pass
else:
    raise AssertionError("a plain local took arguments")


# Many locals touched by one thread, and locals that come and go.
def many(n):
    locals_ = [threading.local() for _ in range(40)]
    for i, l in enumerate(locals_):
        l.i = i
    acc = 0
    for _ in range(n // 40):
        for l in locals_:
            acc += l.i
    return acc


assert many(N) == (N // 40) * sum(range(40))
for i in range(300):
    fresh = threading.local()
    fresh.i = i
    assert fresh.i == i and fresh.__dict__ == {"i": i}


# What a thread parked is released when it ends, and what a local held
# when the local goes away.
class Token:
    pass


refs = []


def park():
    token = Token()
    loc.token = token
    settings.token = token
    refs.append(weakref.ref(token))


t = threading.Thread(target=park)
t.start()
t.join()
del t
gc.collect()
assert refs[0]() is None
assert not hasattr(loc, "token") and not hasattr(settings, "token")

temp = threading.local()
token = Token()
temp.token = token
ref = weakref.ref(token)
del token, temp
gc.collect()
assert ref() is None

# A value replaced or deleted is released then.
token = Token()
loc.token = token
ref = weakref.ref(token)
del token
loc.token = None
gc.collect()
assert ref() is None
del loc.token


# -- queue.SimpleQueue --------------------------------------------------------

sq = queue.SimpleQueue()


def pump(n):
    acc = 0
    for i in range(n):
        sq.put(i)
        acc += sq.get()
    return acc


def pump_nowait(n):
    acc = 0
    for i in range(n):
        sq.put_nowait(i)
        sq.put(i, False)
        sq.put(i, block=True, timeout=None)
        acc += sq.get_nowait() + sq.get(False) + sq.get(block=False)
    return acc


for _ in range(3):
    assert pump(N) == N * (N - 1) // 2
    assert pump_nowait(N) == 3 * N * (N - 1) // 2
assert sq.empty() and sq.qsize() == 0
for i in range(10):
    sq.put(i)
assert sq.qsize() == 10 and not sq.empty()
assert [sq.get() for _ in range(5)] == [0, 1, 2, 3, 4]
assert [sq.get(True, 1.0), sq.get(timeout=1.0), sq.get(True, None)] == [5, 6, 7]
assert [sq.get_nowait(), sq.get(False, 5)] == [8, 9]


def empties(n):
    hits = 0
    for _ in range(n):
        try:
            sq.get_nowait()
        except queue.Empty:
            hits += 1
        try:
            sq.get(False)
        except queue.Empty:
            hits += 1
        try:
            sq.get(block=False)
        except queue.Empty:
            hits += 1
    return hits


assert empties(500) == 1500
started = time.monotonic()
try:
    sq.get(timeout=0.05)
except queue.Empty:
    pass
else:
    raise AssertionError("an empty queue answered")
assert time.monotonic() - started >= 0.04
for bad in (-1, -0.5):
    try:
        sq.get(timeout=bad)
    except ValueError:
        pass
    else:
        raise AssertionError("a negative timeout was taken")
sq.put("x")
try:
    sq.get(timeout=-1)
except ValueError:
    pass
else:
    raise AssertionError("a negative timeout was taken with an item waiting")
assert sq.get() == "x"


# Getters that wait, woken in turn by another thread's puts.
got = []


def consumer(n):
    for _ in range(n):
        got.append(sq.get())


consumers = [threading.Thread(target=consumer, args=(50,)) for _ in range(4)]
for t in consumers:
    t.start()
time.sleep(0.02)
for i in range(200):
    sq.put(i)
    if i % 50 == 0:
        time.sleep(0.005)
for t in consumers:
    t.join(10)
    assert not t.is_alive()
assert sorted(got) == list(range(200)) and sq.empty()

# Producers and consumers at full speed.
got.clear()
producers = [
    threading.Thread(target=lambda base=base: [sq.put(base + i) for i in range(500)])
    for base in (0, 1000, 2000)
]
consumers = [threading.Thread(target=consumer, args=(500,)) for _ in range(3)]
for t in producers + consumers:
    t.start()
for t in producers + consumers:
    t.join(20)
    assert not t.is_alive()
assert sorted(got) == sorted(base + i for base in (0, 1000, 2000) for i in range(500))


# A subclass, and the methods as values.
class Counting(queue.SimpleQueue):
    def __init__(self):
        super().__init__()
        self.gets = 0

    def get(self, block=True, timeout=None):
        self.gets += 1
        return super().get(block, timeout)


cq = Counting()
for i in range(N):
    cq.put(i)
assert sum(cq.get() for _ in range(N)) == N * (N - 1) // 2 and cq.gets == N
put, get = sq.put, sq.get
put(1)
assert get() == 1
assert queue.SimpleQueue.put is not None and callable(queue.SimpleQueue.get_nowait)
alias = queue.SimpleQueue[int]
assert alias.__origin__ is queue.SimpleQueue

print("ok")
