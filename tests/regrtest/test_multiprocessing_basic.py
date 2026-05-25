"""RFC 0024 — `multiprocessing` module surface.

The Rust core for `_multiprocessing` is still a stub: it exposes
`SemLock`, connection objects, and `_get_command`, but real
process creation, pickle-based message passing, and shared memory
are deferred to RFC 0026. So this test only covers the parts that
are wired up: object construction, the in-process Lock/Event
delegates, single-process Queue/Pipe, the cooperative Pool, and
the Manager dict.
"""

import multiprocessing


# ---------------------------------------------------------------------------
# cpu_count / current_process / active_children.
# ---------------------------------------------------------------------------

assert isinstance(multiprocessing.cpu_count(), int)
assert multiprocessing.cpu_count() >= 1

assert multiprocessing.current_process().name
assert isinstance(multiprocessing.active_children(), list)


# ---------------------------------------------------------------------------
# Lock / RLock / Event — backed by the threading equivalents in this
# RFC.
# ---------------------------------------------------------------------------

lock = multiprocessing.Lock()
lock.acquire()
lock.release()

rlock = multiprocessing.RLock()
rlock.acquire()
rlock.release()

event = multiprocessing.Event()
assert not event.is_set()
event.set()
assert event.is_set()


# ---------------------------------------------------------------------------
# Semaphore / BoundedSemaphore.
# ---------------------------------------------------------------------------

sem = multiprocessing.Semaphore(2)
sem.acquire()
sem.release()

bsem = multiprocessing.BoundedSemaphore(1)
bsem.acquire()
bsem.release()


# ---------------------------------------------------------------------------
# Queue / Pipe — single-process today, swapped for shared-memory
# variants in RFC 0026.
# ---------------------------------------------------------------------------

q = multiprocessing.Queue()
q.put("a")
q.put("b")
assert q.get() == "a"
assert q.get() == "b"
assert q.empty()

a, b = multiprocessing.Pipe()
a.send("hello")
assert b.recv() == "hello"
a.close()
b.close()


# ---------------------------------------------------------------------------
# Pool.map (cooperative).
# ---------------------------------------------------------------------------


def double(x):
    return x * 2


with multiprocessing.Pool(2) as pool:
    assert pool.map(double, [1, 2, 3, 4]) == [2, 4, 6, 8]


# ---------------------------------------------------------------------------
# Manager.dict — currently an alias for a regular dict.
# ---------------------------------------------------------------------------

m = multiprocessing.Manager()
d = m.dict()
d["a"] = 1
assert d["a"] == 1


print("multiprocessing surface ok")
