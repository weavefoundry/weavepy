"""RFC 0077 — `queue.SimpleQueue.put` is safe to call from a signal
handler that interrupts a `get` on the same thread.

CPython's `_queue.SimpleQueue` is C and documents that as the point of
the accelerator ("for speed and reentrancy"): `put` appends and releases
a bare wake gate, never taking a lock. The pure-Python
`queue._PySimpleQueue` counts with a `threading.Semaphore`, whose
`release()` takes the semaphore's Condition lock, so a `put` from a
handler that fires while the interrupted `get` holds that lock deadlocks
the thread. WeavePy's `_queue` shim had the Semaphore design. gunicorn's
arbiter is the pattern in the wild: every signal handler is
`SIG_QUEUE.put_nowait(sig)` and the master loop lives in
`SIG_QUEUE.get(timeout=1.0)` / `get_nowait()`; a SIGCHLD during the
SIGTERM shutdown's `get_nowait()` polling hung the master for good
(ecosystem lane, gunicorn_gevent, Linux).

The storm below is a few thousand `get_nowait()` calls under a stream of
SIGUSR1s whose handler does `put_nowait`. Under the old shim it wedged
within the first second on every run; a wedge here surfaces as the
regrtest per-file timeout.
"""

import os
import queue
import signal
import sys
import threading
import time

if not hasattr(signal, "SIGUSR1") or sys.platform == "win32":
    print("rfc0077-simplequeue-signal-reentrancy: skipped (no SIGUSR1)")
    sys.exit(0)

q = queue.SimpleQueue()
delivered = 0


def _handler(signum, frame):
    global delivered
    delivered += 1
    q.put_nowait(signum)


signal.signal(signal.SIGUSR1, _handler)

pid = os.getpid()
stop = threading.Event()


def _storm():
    while not stop.is_set():
        os.kill(pid, signal.SIGUSR1)
        time.sleep(0.0002)


storm = threading.Thread(target=_storm, daemon=True)
storm.start()

# Non-blocking gets: the window where the old design held the Condition
# lock on this thread while a handler needed it.
got = 0
deadline = time.monotonic() + 1.5
while time.monotonic() < deadline:
    try:
        q.get_nowait()
        got += 1
    except queue.Empty:
        pass

# Blocking gets with a timeout: a handler's `put` must wake the parked
# getter (or the item must be there for the next call), never wedge it.
deadline = time.monotonic() + 1.0
while time.monotonic() < deadline:
    try:
        q.get(timeout=0.01)
        got += 1
    except queue.Empty:
        pass

stop.set()
storm.join()
signal.signal(signal.SIGUSR1, signal.SIG_DFL)

# Drain whatever the last handlers queued; every delivered signal shows up.
while True:
    try:
        q.get_nowait()
        got += 1
    except queue.Empty:
        break
assert delivered > 0, "no signals were delivered"
assert got == delivered, (got, delivered)

# The gate discipline survives a timed-out wait: a `put` after `get` gave
# up must not error (`release unlocked lock`) and the item is delivered.
q2 = queue.SimpleQueue()
try:
    q2.get(timeout=0.01)
except queue.Empty:
    pass
q2.put_nowait("late")
assert q2.get_nowait() == "late"

# And a parked getter on another thread is woken by `put`.
q3 = queue.SimpleQueue()
result = []


def _wait():
    result.append(q3.get(timeout=5))


t = threading.Thread(target=_wait)
t.start()
time.sleep(0.05)
q3.put("x")
t.join(timeout=5)
assert result == ["x"], result

print("rfc0077-simplequeue-signal-reentrancy: ok", got, "signals")
