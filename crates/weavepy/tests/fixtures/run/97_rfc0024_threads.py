"""RFC 0024 — real `threading` primitives over the new `_thread`."""

import threading


# Lock
lock = threading.Lock()
print("lock starts unlocked:", not lock.locked())
lock.acquire()
print("lock locked:", lock.locked())
lock.release()
print("lock released:", not lock.locked())

with lock:
    print("inside lock context:", lock.locked())
print("after lock context:", not lock.locked())


# RLock
rlock = threading.RLock()
rlock.acquire()
rlock.acquire()
rlock.release()
rlock.release()
print("rlock fully released")


# Event
ev = threading.Event()
print("event starts cleared:", not ev.is_set())
ev.set()
print("event set:", ev.is_set())
ev.clear()
print("event cleared:", not ev.is_set())


# Semaphore
sem = threading.Semaphore(2)
sem.acquire()
sem.acquire()
print("non-blocking on depleted:", sem.acquire(blocking=False))
sem.release()
sem.release()


# BoundedSemaphore — over-release raises
bsem = threading.BoundedSemaphore(2)
bsem.acquire()
bsem.release()
try:
    bsem.release()
    bsem.release()
    print("over-release missed")
except ValueError:
    print("bounded semaphore raised on over-release")


# Condition (single-thread degenerate case)
cond = threading.Condition()
with cond:
    cond.notify()
    cond.notify_all()
print("condition no-op notify ok")


# Thread bookkeeping
def worker(out, value):
    out.append(value)


out = []
threads = [threading.Thread(target=worker, args=(out, i)) for i in range(5)]
for t in threads:
    t.start()
for t in threads:
    t.join()
print("worker output:", sorted(out))


# Module-level functions
print("ident type:", type(threading.get_ident()).__name__)
print("active_count >= 1:", threading.active_count() >= 1)
print("main thread name:", threading.main_thread().name)
