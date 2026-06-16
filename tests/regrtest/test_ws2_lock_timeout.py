"""RFC 0039 WS2 — lock/RLock timeout + cross-thread hand-off fidelity.

`_thread.LockType.acquire(blocking, timeout)` honours a real wall-clock
timeout and drops the GIL while parked, so a worker holding the lock can make
progress and a timed acquire on the main thread returns `False` rather than
deadlocking the interpreter. `RLock` tracks owner + recursion and rejects a
release by a non-owner.
"""

import threading
import time


# ---------------------------------------------------------------------------
# Non-blocking + timed acquire on an already-held lock.
# ---------------------------------------------------------------------------

lock = threading.Lock()
assert lock.acquire()
assert lock.locked()

assert lock.acquire(blocking=False) is False

start = time.monotonic()
assert lock.acquire(timeout=0.15) is False
elapsed = time.monotonic() - start
# Real timeout, not an instant return; allow generous slack for CI jitter.
assert elapsed >= 0.1, elapsed
lock.release()
assert not lock.locked()


# ---------------------------------------------------------------------------
# Cross-thread hand-off: a worker holds the lock, the main thread blocks on a
# timed acquire that fails, then succeeds once the worker releases. This only
# works if `acquire` drops the GIL while parked (else the whole interpreter
# would deadlock).
# ---------------------------------------------------------------------------

handoff = threading.Lock()
release_now = threading.Event()
acquired = threading.Event()


def holder():
    handoff.acquire()
    acquired.set()
    # Hold until the main thread tells us to let go.
    release_now.wait(timeout=5)
    handoff.release()


h = threading.Thread(target=holder)
h.start()
assert acquired.wait(timeout=5), "worker never acquired the lock"

# Worker holds it: a short timed acquire must fail.
assert handoff.acquire(timeout=0.1) is False

# Tell the worker to release, then a blocking acquire must succeed.
release_now.set()
assert handoff.acquire(timeout=5) is True
handoff.release()
h.join()


# ---------------------------------------------------------------------------
# RLock — reentrant for the owner, RuntimeError on release-by-non-owner and
# release-when-unlocked.
# ---------------------------------------------------------------------------

rlock = threading.RLock()
assert rlock.acquire()
assert rlock.acquire()  # reentrant
rlock.release()
rlock.release()

try:
    rlock.release()  # not held
    raise AssertionError("release of an unlocked RLock should raise")
except RuntimeError:
    pass

owner_locked = threading.RLock()
owner_locked.acquire()
nonowner_result = []


def stranger():
    try:
        owner_locked.release()
        nonowner_result.append("released")
    except RuntimeError:
        nonowner_result.append("runtime_error")


s = threading.Thread(target=stranger)
s.start()
s.join()
owner_locked.release()
assert nonowner_result == ["runtime_error"], nonowner_result


# ---------------------------------------------------------------------------
# Condition.wait(timeout) returns False on timeout (no notifier).
# ---------------------------------------------------------------------------

cond = threading.Condition()
with cond:
    start = time.monotonic()
    notified = cond.wait(timeout=0.1)
    assert notified is False, notified
    assert time.monotonic() - start >= 0.08


print("WS2 lock/RLock timeout ok")
