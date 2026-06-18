"""RFC 0040 WS2/WS3 — faithful `subprocess.Popen` over `_posixsubprocess.fork_exec`.

Pins the in-process process model this wave landed: real pipe plumbing,
deadlock-free `communicate()`, exit-status handling, `pass_fds` inheritance
through `fork_exec`, the `b"<Exc>:<hexerrno>:<msg>"` errpipe protocol, and
the credential-argument validation done in the parent before the fork.
"""

import os
import signal
import subprocess
import sys


def _find_weavepy():
    # Run standalone, `sys.executable` is already the `weavepy` interpreter.
    # In-process under the conformance harness it is the conformance binary,
    # whose `weavepy` sibling is the real interpreter (the same resolution
    # `--mode subprocess` uses). Returns None if neither is found, so the
    # fixture can skip cleanly rather than spawn the wrong binary.
    exe = sys.executable
    if os.path.basename(exe) == "weavepy":
        return exe
    sib = os.path.join(os.path.dirname(exe), "weavepy")
    return sib if os.path.exists(sib) else None


PY = _find_weavepy()
if PY is None:
    print("WS2/WS3 subprocess process model: skipped (no weavepy interpreter)")
    sys.exit(0)


# ---------------------------------------------------------------------------
# communicate() round-trips stdin -> stdout without deadlocking, even on a
# payload far larger than a single pipe buffer (the selector-driven drain).
# ---------------------------------------------------------------------------

payload = b"weavepy\n" * 20000  # ~160 KiB, exceeds any pipe buffer
p = subprocess.Popen(
    [PY, "-c", "import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
out, err = p.communicate(payload)
assert out == payload, (len(out), len(payload))
assert err == b"", err
assert p.returncode == 0, p.returncode


# ---------------------------------------------------------------------------
# check_output / run / CalledProcessError.
# ---------------------------------------------------------------------------

assert subprocess.check_output([PY, "-c", "print('hi')"]) == b"hi\n"

cp = subprocess.run([PY, "-c", "import sys; sys.exit(3)"])
assert cp.returncode == 3, cp.returncode

try:
    subprocess.check_call([PY, "-c", "import sys; sys.exit(7)"])
except subprocess.CalledProcessError as e:
    assert e.returncode == 7, e.returncode
else:
    raise AssertionError("CalledProcessError not raised")


# ---------------------------------------------------------------------------
# text mode (universal newlines) + env passing.
# ---------------------------------------------------------------------------

child_env = dict(os.environ)
child_env["WP_FIXTURE"] = "ok"
cp = subprocess.run(
    [PY, "-c", "import os; print(os.environ['WP_FIXTURE'])"],
    capture_output=True,
    text=True,
    env=child_env,
)
assert cp.stdout == "ok\n", repr(cp.stdout)


# ---------------------------------------------------------------------------
# pass_fds: a descriptor outside 0/1/2 survives fork_exec's make_inheritable
# + close-fds sweep and is readable in the child.
# ---------------------------------------------------------------------------

r, w = os.pipe()
try:
    os.write(w, b"handshake")
    os.close(w)
    code = "import os,sys; sys.stdout.write(os.read(int(sys.argv[1]), 32).decode())"
    out = subprocess.check_output(
        [PY, "-c", code, str(r)], pass_fds=(r,), text=True
    )
    assert out == "handshake", repr(out)
finally:
    os.close(r)


# ---------------------------------------------------------------------------
# timeout -> TimeoutExpired and the process is killed.
# ---------------------------------------------------------------------------

try:
    subprocess.run([PY, "-c", "import time; time.sleep(30)"], timeout=0.2)
except subprocess.TimeoutExpired:
    pass
else:
    raise AssertionError("TimeoutExpired not raised")


# ---------------------------------------------------------------------------
# send_signal / terminate over the real pid.
# ---------------------------------------------------------------------------

p = subprocess.Popen([PY, "-c", "import time; time.sleep(30)"])
p.terminate()
assert p.wait(timeout=5) == -signal.SIGTERM, p.returncode


# ---------------------------------------------------------------------------
# Parent-side credential validation (uid_t range), before any fork.
# ---------------------------------------------------------------------------

try:
    subprocess.check_call([PY, "-c", "pass"], user=-1)
except ValueError:
    pass
else:
    raise AssertionError("ValueError not raised for user=-1")

try:
    subprocess.check_call([PY, "-c", "pass"], user=2 ** 64)
except OverflowError:
    pass
else:
    raise AssertionError("OverflowError not raised for user=2**64")


# ---------------------------------------------------------------------------
# A non-existent executable reports the executable as OSError.filename.
# ---------------------------------------------------------------------------

try:
    subprocess.Popen(["/nonexistent/weavepy-fixture-cmd"])
except FileNotFoundError as e:
    assert e.filename == "/nonexistent/weavepy-fixture-cmd", e.filename
else:
    raise AssertionError("FileNotFoundError not raised")


print("WS2/WS3 subprocess process model ok")
