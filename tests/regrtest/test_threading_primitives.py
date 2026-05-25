"""RFC 0024 — `threading` primitives backed by real OS threads + GIL.

Drives `_thread.allocate_lock` / `_thread.RLock` / `Event` /
`Semaphore` / `Condition` / `Thread` / `Barrier` and the module-level
helpers (`get_ident`, `active_count`, `main_thread`, `local`). The
fixtures stay degenerate — most of them just need WeavePy to *not*
deadlock or raise — because we don't yet have a way to assert on
real-thread interleavings deterministically.
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
# Thread bookkeeping. `start_new_thread` runs the target cooperatively
# in this RFC, but the join semantics still need to flush the queue.
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
