"""RFC 0059 WS2 — unified eval breaker: pending work must still be
serviced promptly from inside hot loops after the per-instruction
probes were consolidated into the single hot-gates word."""

import sys
import time
import threading


# 1. Finalizer promptness: an object whose only reference is rebound
#    inside a hot loop must run its __del__ *during* the loop (the
#    refcount death is detected at the loop's safe points), not after
#    the loop exits.
fired = []


class Tracked:
    def __del__(self):
        fired.append(True)


def finalizer_in_loop():
    obj = Tracked()
    for i in range(200000):
        obj = None  # drops the only reference on the first iteration
        if fired:
            return i
        obj = i
    return -1


assert finalizer_in_loop() >= 0, "__del__ did not run inside the hot loop"


# 2. Cross-thread async signal delivery: _thread.interrupt_main()
#    trips SIGINT, and the *main* thread must run the (custom, so no
#    exception-catching races — even CPython is flaky there) handler
#    mid-loop via the eval breaker.
import _thread
import signal

sigint_seen = []


def poke():
    time.sleep(0.05)
    _thread.interrupt_main()


old_int = signal.signal(signal.SIGINT, lambda s, f: sigint_seen.append(s))
try:
    t = threading.Thread(target=poke)
    t.start()
    deadline = time.monotonic() + 10.0
    while not sigint_seen and time.monotonic() < deadline:
        pass
    t.join()
finally:
    signal.signal(signal.SIGINT, old_int)
assert sigint_seen, "interrupt_main never delivered to the hot loop"


# 3. GIL fairness: two pure-Python spinner threads must both make
#    progress (the eval breaker's gil-yield countdown is what forces
#    handoffs in loops that never touch a blocking call).
counts = [0, 0]
stop = False


def spinner(slot):
    while not stop:
        counts[slot] += 1


threads = [threading.Thread(target=spinner, args=(i,)) for i in range(2)]
for th in threads:
    th.start()
time.sleep(0.3)
stop = True
for th in threads:
    th.join()
assert counts[0] > 0 and counts[1] > 0, f"a spinner starved: {counts}"

# 4. Signal latency under a hot loop (Unix): a timer signal must land
#    while the loop is spinning.
if hasattr(__import__("signal"), "setitimer"):
    import signal

    got = []
    old = signal.signal(signal.SIGALRM, lambda s, f: got.append(s))
    try:
        signal.setitimer(signal.ITIMER_REAL, 0.05)
        deadline = time.monotonic() + 10.0
        while not got and time.monotonic() < deadline:
            pass
        assert got, "SIGALRM never interrupted the hot loop"
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0.0)
        signal.signal(signal.SIGALRM, old)

print("ok")
