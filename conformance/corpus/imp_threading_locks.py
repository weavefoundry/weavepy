"""RFC 0024 — `threading` Lock / RLock / Event / Semaphore primitives."""

import threading


lock = threading.Lock()
print(lock.acquire())
print(lock.locked())
lock.release()
print(lock.locked())

with lock:
    pass
print(lock.locked())


rlock = threading.RLock()
rlock.acquire()
rlock.acquire()
rlock.release()
rlock.release()


event = threading.Event()
print(event.is_set())
event.set()
print(event.is_set())


sem = threading.Semaphore(2)
sem.acquire()
sem.acquire()
print(sem.acquire(blocking=False))
sem.release()
sem.release()


bsem = threading.BoundedSemaphore(2)
bsem.acquire()
bsem.release()
try:
    bsem.release()
    bsem.release()
    print("missed")
except ValueError:
    print("over-release rejected")
