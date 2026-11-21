"""A contended lock's waiter gets its turn.

A waiter for a `threading.Lock` (or `RLock`) waits for the lock to come
free with the GIL released and takes it once the GIL is back, so the
running thread can retake a lock it released instead of switching
threads at every acquire. A holder that has the lock nearly all the time
when it yields the GIL must still not starve the waiter: past a few
milliseconds the waiter takes the lock as soon as it comes free.
"""

import threading
import time


def check(lock):
    stop = False
    rounds = [0]

    def hog():
        while not stop:
            with lock:
                total = 0
                for i in range(2000):
                    total += i
                rounds[0] += 1

    t = threading.Thread(target=hog)
    t.start()
    try:
        start = time.monotonic()
        got = 0
        while time.monotonic() - start < 0.3:
            with lock:
                got += 1
        elapsed = time.monotonic() - start
    finally:
        stop = True
        t.join()
    assert got >= 3, (type(lock).__name__, got)
    assert rounds[0] >= 3, rounds[0]
    # Each acquire waits a few milliseconds at most.
    assert elapsed < 3.0, elapsed


check(threading.Lock())
check(threading.RLock())
