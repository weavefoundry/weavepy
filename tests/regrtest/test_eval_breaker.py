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


# 5. RFC 0067 WS2 — the same guarantees inside the tier-2 JIT's
#    *native* loops (the default build ships with the JIT on). This
#    kernel is deliberately JIT-shaped: typed int locals, a burnable
#    global flag, no calls. The handler rebinds the guarded global;
#    the native loop's eval-breaker poll must deopt for the pending
#    signal and observe the rebind. The huge bound only matters when
#    the machinery is broken (the loop then completes and the assert
#    fires) — the healthy path exits within one poll stride.
if hasattr(__import__("signal"), "setitimer"):
    import signal

    alrm_hit = False

    def _on_alrm(s, f):
        global alrm_hit
        alrm_hit = True

    def native_spin(limit):
        i = 0
        while i < limit:
            if alrm_hit:
                return i
            i = i + 1
        return i

    old = signal.signal(signal.SIGALRM, _on_alrm)
    try:
        signal.setitimer(signal.ITIMER_REAL, 0.05)
        r = native_spin(4000000000)
        assert r < 4000000000, "SIGALRM never interrupted the native loop"
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0.0)
        signal.signal(signal.SIGALRM, old)


# 6. RFC 0067 WS2 — KeyboardInterrupt *into* a native loop: a second
#    thread trips SIGINT while the main thread spins in a JIT-shaped
#    kernel with no early exit. The only way out before the bound is
#    the eval breaker servicing the signal (default handler raises
#    KeyboardInterrupt at the loop's deopt point) — which also proves
#    the poll hands off the GIL, or the poking thread could never run.
def ki_spin(limit):
    i = 0
    while i < limit:
        i = i + 1
    return i


def _poke_ki():
    time.sleep(0.05)
    _thread.interrupt_main()


_t = threading.Thread(target=_poke_ki)
_t.start()
_ki_raised = False
try:
    ki_spin(4000000000)
except KeyboardInterrupt:
    _ki_raised = True
_t.join()
assert _ki_raised, "KeyboardInterrupt never landed in the native loop"

print("ok")
