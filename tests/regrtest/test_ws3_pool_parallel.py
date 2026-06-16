"""RFC 0039 WS3 — `concurrent.futures.ThreadPoolExecutor` real parallelism.

Work items dispatch to real worker threads via a blocking queue; `Future`
results synchronise back to the submitter, and `map`/`as_completed`/`wait`
block correctly. The pool counts concurrent occupancy to prove the workers
run at the same time (not serialised by the submit loop).
"""

import threading
import time
from concurrent.futures import (
    ALL_COMPLETED,
    FIRST_COMPLETED,
    ThreadPoolExecutor,
    as_completed,
    wait,
)


# ---------------------------------------------------------------------------
# map preserves input order over its results.
# ---------------------------------------------------------------------------

def square(x):
    return x * x


with ThreadPoolExecutor(max_workers=4) as ex:
    assert list(ex.map(square, range(8))) == [0, 1, 4, 9, 16, 25, 36, 49]


# ---------------------------------------------------------------------------
# submit + result + as_completed.
# ---------------------------------------------------------------------------

with ThreadPoolExecutor(max_workers=4) as ex:
    futs = [ex.submit(square, i) for i in range(10)]
    assert sorted(f.result(timeout=5) for f in as_completed(futs)) == [
        0, 1, 4, 9, 16, 25, 36, 49, 64, 81
    ]


# ---------------------------------------------------------------------------
# Real concurrency: 4 tasks each sleep while a shared counter tracks peak
# occupancy. With 4 workers the peak must exceed 1 (they overlap).
# ---------------------------------------------------------------------------

lock = threading.Lock()
current = 0
peak = 0
start_barrier = threading.Barrier(4)


def occupy():
    global current, peak
    start_barrier.wait(timeout=5)  # force all 4 to be in-flight together
    with lock:
        current += 1
        peak = max(peak, current)
    time.sleep(0.05)
    with lock:
        current -= 1
    return True


with ThreadPoolExecutor(max_workers=4) as ex:
    results = list(ex.map(lambda _: occupy(), range(4)))
assert all(results)
assert peak >= 2, f"expected overlapping workers, peak occupancy was {peak}"


# ---------------------------------------------------------------------------
# wait() with FIRST_COMPLETED / ALL_COMPLETED.
# ---------------------------------------------------------------------------

def slow(n):
    time.sleep(n)
    return n


with ThreadPoolExecutor(max_workers=3) as ex:
    fs = [ex.submit(slow, d) for d in (0.01, 0.05, 0.2)]
    done, not_done = wait(fs, return_when=FIRST_COMPLETED)
    assert len(done) >= 1
    done2, not_done2 = wait(fs, return_when=ALL_COMPLETED, timeout=5)
    assert len(done2) == 3 and not not_done2


# ---------------------------------------------------------------------------
# Exceptions propagate through Future.result().
# ---------------------------------------------------------------------------

def boom():
    raise ValueError("kaboom")


with ThreadPoolExecutor(max_workers=2) as ex:
    fut = ex.submit(boom)
    try:
        fut.result(timeout=5)
        raise AssertionError("exception should have propagated")
    except ValueError as e:
        assert str(e) == "kaboom"


print("WS3 ThreadPoolExecutor parallelism ok")
