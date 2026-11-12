"""A signal a native call trips (`os.kill` of this process) runs its handler
as the call returns, from compiled loops too: a handler that raises raises
from the call's line, and one that records sees the call's caller. Also
`str.format`'s attribute trailers, called directly and through a spread."""

import os
import signal
import sys
import traceback

if not hasattr(signal, "SIGUSR1"):
    print("ok")
    raise SystemExit


class Tripped(Exception):
    pass


def raising(signum, frame):
    raise Tripped(signum)


seen = []


def recording(signum, frame):
    seen.append(frame.f_code.co_name)


def kill_loop(n, at):
    count = 0
    for i in range(n):
        if i == at:
            os.kill(os.getpid(), signal.SIGUSR1)  # the killing line
            count += 1000
        count += 1
    return count


old = signal.signal(signal.SIGUSR1, raising)
try:
    for _ in range(4):
        try:
            kill_loop(500, 499)
        except Tripped:
            tb = traceback.extract_tb(sys.exc_info()[2])
            assert any(t.name == "kill_loop" and "the killing line" in (t.line or "")
                       for t in tb), tb
        else:
            raise AssertionError("expected Tripped")
    signal.signal(signal.SIGUSR1, recording)
    for _ in range(3):
        seen.clear()
        assert kill_loop(300, 10) == 1300
        assert seen == ["kill_loop"], seen
finally:
    signal.signal(signal.SIGUSR1, old)


# `str.format` attribute trailers.
for _ in range(3):
    for fmt, args, want in (
        ("{0.real}", (5,), "5"),
        ("{0.imag}", (2.5,), "0.0"),
        ("{0.__class__.__name__}", ("s",), "str"),
    ):
        assert fmt.format(*args) == want, (fmt, fmt.format(*args))
        f = fmt.format
        assert f(*args, **{}) == want
    for bad in ("{0.}", "{0.}"):
        try:
            bad.format(0)
        except ValueError as e:
            assert str(e) == "Empty attribute in format string", e
        else:
            raise AssertionError("expected ValueError")
        f = bad.format
        try:
            f(*(0,), **{})
        except ValueError:
            pass
        else:
            raise AssertionError("expected ValueError")
print("ok")
