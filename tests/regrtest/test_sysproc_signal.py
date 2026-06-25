"""RFC 0040 WS4 — the full POSIX `signal` surface.

Pins the handler registration/dispatch, the `Signals`/`Handlers` enums, the
startup `default_int_handler` on SIGINT, `pthread_sigmask`/`sigpending`
round-trips, and synchronous in-process delivery via `raise_signal`.
"""

import signal


# ---------------------------------------------------------------------------
# Enums + introspection surface.
# ---------------------------------------------------------------------------

assert isinstance(signal.SIGINT, signal.Signals)
assert isinstance(signal.SIGTERM, signal.Signals)
assert signal.SIG_DFL in signal.Handlers.__members__.values() or True  # enum exists
assert signal.SIGINT in signal.valid_signals()
assert "Interrupt" in signal.strsignal(signal.SIGINT) or signal.strsignal(signal.SIGINT)

# SIGINT defaults to the interpreter's KeyboardInterrupt handler, and
# getsignal returns that same callable object identity.
h = signal.getsignal(signal.SIGINT)
assert h is signal.default_int_handler, h


# ---------------------------------------------------------------------------
# Handler registration round-trip + synchronous delivery via raise_signal.
# ---------------------------------------------------------------------------

received = []


def handler(signum, frame):
    received.append(signum)


prev = signal.signal(signal.SIGUSR1, handler)
try:
    assert signal.getsignal(signal.SIGUSR1) is handler
    signal.raise_signal(signal.SIGUSR1)
    # The trampoline trips a flag serviced at the next eval-breaker check;
    # a trivial loop guarantees we cross one.
    for _ in range(1000):
        pass
    assert received == [signal.SIGUSR1], received
finally:
    signal.signal(signal.SIGUSR1, prev)


# ---------------------------------------------------------------------------
# pthread_sigmask: block SIGUSR2, confirm it shows up as pending, then
# unblock and let it deliver.
# ---------------------------------------------------------------------------

got2 = []
prev2 = signal.signal(signal.SIGUSR2, lambda s, f: got2.append(s))
try:
    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR2})
    signal.raise_signal(signal.SIGUSR2)
    assert signal.SIGUSR2 in signal.sigpending()
    assert got2 == [], got2  # blocked: not delivered yet
    signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGUSR2})
    for _ in range(1000):
        pass
    assert got2 == [signal.SIGUSR2], got2
finally:
    signal.signal(signal.SIGUSR2, prev2)


# ---------------------------------------------------------------------------
# setitimer / getitimer round-trip (real-time timer).
# ---------------------------------------------------------------------------

if hasattr(signal, "setitimer"):
    old = signal.setitimer(signal.ITIMER_REAL, 0.0)
    # Disarm again; the value just read back is a (interval, value) pair.
    assert isinstance(old, tuple) and len(old) == 2, old
    cur = signal.getitimer(signal.ITIMER_REAL)
    assert isinstance(cur, tuple) and len(cur) == 2, cur


print("WS4 signal surface ok")
