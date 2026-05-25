"""RFC 0024 + 0025 — `threading` primitives backed by real OS threads + GIL.

Drives `_thread.allocate_lock` / `_thread.RLock` / `Event` /
`Semaphore` / `Condition` / `Thread` / `Barrier` and the module-level
helpers (`get_ident`, `active_count`, `main_thread`, `local`). Under
RFC 0025 the workers run on real OS threads and share the heap with
the parent, so the fixture also asserts cross-thread mutation
visibility (`shared.append` in a worker is observable to the parent
after `join`).
"""

import threading


# ---------------------------------------------------------------------------
# Lock — non-reentrant, owner-tracked.
# ---------------------------------------------------------------------------

lock = threading.Lock()
assert not lock.locked()
assert lock.acquire()
assert lock.locked()
lock.release()
assert not lock.locked()

with lock:
    assert lock.locked()
assert not lock.locked()

assert lock.acquire(blocking=False)
assert not lock.acquire(blocking=False)
lock.release()


# ---------------------------------------------------------------------------
# RLock — reentrant.
# ---------------------------------------------------------------------------

rlock = threading.RLock()
rlock.acquire()
rlock.acquire()
rlock.acquire()
rlock.release()
rlock.release()
rlock.release()


# ---------------------------------------------------------------------------
# Event.
# ---------------------------------------------------------------------------

ev = threading.Event()
assert not ev.is_set()
ev.set()
assert ev.is_set()
ev.clear()
assert not ev.is_set()
ev.set()
assert ev.wait(timeout=0)


# ---------------------------------------------------------------------------
# Semaphore + BoundedSemaphore.
# ---------------------------------------------------------------------------

sem = threading.Semaphore(3)
assert sem.acquire()
assert sem.acquire()
assert sem.acquire()
assert not sem.acquire(blocking=False)
sem.release()
sem.release()
sem.release()

bsem = threading.BoundedSemaphore(2)
bsem.acquire()
bsem.release()
try:
    bsem.release()
    bsem.release()
    raise AssertionError("over-release should have raised")
except ValueError:
    pass


# ---------------------------------------------------------------------------
# Condition (degenerate single-thread case — notify with no waiters
# should be a fast no-op).
# ---------------------------------------------------------------------------

cond = threading.Condition()
with cond:
    cond.notify()
    cond.notify_all()


# ---------------------------------------------------------------------------
# Thread bookkeeping. RFC 0025 — workers run on real OS threads and
# share the heap with the parent; mutations to `out` from inside the
# worker target must be observable after `join()`.
# ---------------------------------------------------------------------------


def worker(out, value):
    out.append(value)


out = []
threads = [threading.Thread(target=worker, args=(out, i)) for i in range(5)]
for t in threads:
    t.start()
for t in threads:
    t.join()

assert sorted(out) == [0, 1, 2, 3, 4], out

# Cross-thread lock contention — 4 threads × 100 increments under a
# shared `threading.Lock` should deterministically land on 400.
counter = [0]
counter_lock = threading.Lock()


def bump(n):
    for _ in range(n):
        with counter_lock:
            counter[0] += 1


workers = [threading.Thread(target=bump, args=(100,)) for _ in range(4)]
for w in workers:
    w.start()
for w in workers:
    w.join()
assert counter[0] == 400, counter[0]

# `Thread.is_alive()` must flip to False after `join()` — the worker's
# `_tstate_lock` release is the sentinel that drives this transition.
def short():
    pass


sh = threading.Thread(target=short)
sh.start()
sh.join()
assert not sh.is_alive()


# ---------------------------------------------------------------------------
# threading.local — per-thread storage.
# ---------------------------------------------------------------------------

local = threading.local()
local.x = 42
assert local.x == 42


# ---------------------------------------------------------------------------
# Barrier — degenerate single-party case.
# ---------------------------------------------------------------------------

barrier = threading.Barrier(1)
assert barrier.wait() == 0


# ---------------------------------------------------------------------------
# Module-level functions.
# ---------------------------------------------------------------------------

assert isinstance(threading.get_ident(), int)
assert isinstance(threading.active_count(), int)
assert threading.active_count() >= 1
assert threading.main_thread().name == "MainThread"

print("threading primitives ok")
