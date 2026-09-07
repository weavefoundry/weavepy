"""RFC 0077 — one monotonic clock per process, and sub-millisecond socket
timeouts wait their full budget.

Both surfaced from the same 3.14 sweep flake
(`test_httpservers.CGIHTTPServerTestCase.test_large_content_length_truncated`,
1 ms handler timeout, failing under parallel load only on WeavePy):

* `time.monotonic()` / `time.perf_counter()` kept a per-thread origin, so
  a thread started later read a smaller clock than the main thread by the
  start delay. Every cross-thread deadline comparison inherits that skew.
* The socket readiness wait truncated the remaining budget to whole
  milliseconds before `poll(2)`, so a 1 ms timeout became `poll(.., 0)`
  (an instant `TimeoutError`) as soon as any time had elapsed since the
  deadline was taken. CPython rounds the budget up.
"""

import socket
import threading
import time


# --- one clock for every thread -------------------------------------------

main_mono = time.monotonic()
main_perf = time.perf_counter()
main_perf_ns = time.perf_counter_ns()
time.sleep(0.05)

seen = {}


def _read():
    seen["mono"] = time.monotonic()
    seen["perf"] = time.perf_counter()
    seen["perf_ns"] = time.perf_counter_ns()


t = threading.Thread(target=_read)
t.start()
t.join()
assert seen["mono"] - main_mono >= 0.04, (seen["mono"], main_mono)
assert seen["perf"] - main_perf >= 0.04, (seen["perf"], main_perf)
assert seen["perf_ns"] - main_perf_ns >= 40_000_000, (seen["perf_ns"], main_perf_ns)
# Still monotonic from the main thread's point of view afterwards.
assert time.monotonic() >= seen["mono"]


# --- a 1 ms socket timeout waits about 1 ms, then raises ------------------

a, b = socket.socketpair()
try:
    a.settimeout(0.001)
    for _ in range(20):
        t0 = time.perf_counter()
        try:
            a.recv(1)
        except TimeoutError:
            pass
        else:
            raise AssertionError("recv on an empty socket returned")
        elapsed = time.perf_counter() - t0
        # Truncation made this a few microseconds; the ceiling makes it a
        # real ~1 ms wait. Leave slack for the scheduler on the high side.
        assert elapsed >= 0.0005, elapsed
        assert elapsed < 1.0, elapsed

    # And data that arrives inside the window is still delivered.
    def _late_writer():
        b.sendall(b"x")

    a.settimeout(0.5)
    w = threading.Thread(target=_late_writer)
    w.start()
    assert a.recv(1) == b"x"
    w.join()
finally:
    a.close()
    b.close()

print("rfc0077-clock-and-socket-timeout: ok")
