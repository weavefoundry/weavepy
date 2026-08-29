"""RFC 0075 WS10 — engine fixes surfaced by the endgame re-baseline grade.

One bundled canary per fix:

1. `os.supports_fd` advertises `os.utime` (the implementation always
   accepted an fd, but the capability set never said so — test_os's
   `test_utime_invalid_arguments` then expected a TypeError the call
   correctly didn't raise), and `os.utime(fd)` round-trips.

2. `os.getgroups()` exists and returns the supplementary group ids
   (test_subprocess's `extra_groups` legs probe membership through it
   in the child).

3. `print()` with `sys.stdout = None` is a silent no-op — CPython's
   PyFile_WriteObject contract during interpreter teardown (test_
   subprocess hits it via `-S` children) — instead of AttributeError
   on `None.write`.

4. `zlib.compressobj`/`zlib.decompressobj` reject out-of-range `wbits`
   with ValueError ("Invalid initialization option", CPython's
   Z_STREAM_ERROR surface), not `zlib.error` (test_zlib
   `test_badcompressobj`/`test_baddecompressobj`).

5. `subprocess.Popen` credential validation fires in the *parent*:
   `group=-1` -> ValueError, `group=2**64` -> OverflowError,
   `extra_groups=[-1]` -> ValueError (the `_Py_Gid_Converter`
   contract), and `extra_groups=[]` genuinely calls `setgroups(0, …)`
   in the child — dropping every supplementary group as root,
   surfacing PermissionError(EPERM) with `filename is None`
   unprivileged — rather than silently skipping the call
   (test_subprocess `test_group`/`test_extra_groups*`).

6. Teardown smoke for the WS9 dealloc guards: instances of a
   Python-defined subclass of an extension type (a faithful C body)
   created and dropped on worker threads, plus one left alive to
   process exit. The WS9 `BODY_FREE_IN_FLIGHT`/`BODY_FREE_CONSENT`
   reentrancy guards are thread-local; an `Object` drop running from
   thread-exit TLS destruction (a parked C-drop queue dying with the
   thread) must degrade to a leak, not panic-abort inside `run_dtors`
   ("cannot access a Thread Local Storage value during or after
   destruction" — the lxml/gevent/psycopg probe aborts). The full
   shape needs a real extension wheel; the ecosystem probe rows are
   the strong net, this is the in-tree smoke.
"""

import json
import os
import subprocess
import sys
import tempfile
import zlib

# ------------------- 1. os.supports_fd covers utime ---------------------

assert os.utime in os.supports_fd, sorted(f.__name__ for f in os.supports_fd)
with tempfile.NamedTemporaryFile() as fp:
    os.utime(fp.fileno(), times=(123456789, 123456789))
    st = os.stat(fp.name)
    assert int(st.st_mtime) == 123456789, st.st_mtime

# ------------------- 2. os.getgroups --------------------------------------

_groups = os.getgroups()
assert isinstance(_groups, list), type(_groups)
assert all(isinstance(g, int) and g >= 0 for g in _groups), _groups

# ------------------- 3. print() into sys.stdout = None --------------------

_saved_stdout = sys.stdout
try:
    sys.stdout = None
    print("swallowed")  # must not raise
finally:
    sys.stdout = _saved_stdout

# ------------------- 4. zlib bad-wbits ValueError --------------------------

for bad in (lambda: zlib.compressobj(1, zlib.DEFLATED, 0),
            lambda: zlib.compressobj(1, zlib.DEFLATED, 100),
            lambda: zlib.decompressobj(-1),
            lambda: zlib.decompressobj(100)):
    try:
        bad()
    except ValueError:
        pass
    else:
        raise AssertionError(f"{bad} did not raise ValueError")

# ------------------- 5. subprocess credential validation -------------------

_cmd = [sys.executable, "-c", "pass"]
if hasattr(os, "setregid"):
    for exc, kwargs in (
        (ValueError, {"group": -1}),
        (OverflowError, {"group": 2 ** 64}),
        (ValueError, {"extra_groups": [-1]}),
        (ValueError, {"extra_groups": [2 ** 64]}),
    ):
        try:
            subprocess.check_call(_cmd, **kwargs)
        except exc:
            pass
        else:
            raise AssertionError(f"Popen(**{kwargs}) did not raise {exc.__name__}")

if hasattr(os, "setgroups"):
    try:
        _out = subprocess.check_output(
            [sys.executable, "-c",
             "import os, sys, json; json.dump(os.getgroups(), sys.stdout)"],
            extra_groups=[])
    except PermissionError as exc:
        # Unprivileged: the child's setgroups(0, …) EPERM must surface
        # as a filename-less PermissionError, not be swallowed.
        assert exc.filename is None, exc.filename
    else:
        assert json.loads(_out) == [], _out

# ------------------- 6. thread/process-exit dealloc smoke ------------------

import threading

try:
    import _testbuffer
except ImportError:
    _testbuffer = None

if _testbuffer is not None:
    class _NDSub(_testbuffer.ndarray):
        pass

    def _worker(keep):
        # Faithful-body instances crossing into C (memoryview walks the
        # buffer protocol through the extension base), dying at thread
        # end in varied orders: some dropped mid-thread, one parked in
        # the caller's list so it outlives the thread.
        local = []
        for i in range(8):
            nd = _NDSub([i] * 4, shape=[4], format='L')
            with memoryview(nd) as m:
                assert m[0] == i
            local.append(nd)
        keep.append(local.pop())

    _keep = []
    _threads = [threading.Thread(target=_worker, args=(_keep,)) for _ in range(4)]
    for t in _threads:
        t.start()
    for t in _threads:
        t.join()
    assert len(_keep) == 4
    # One survivor rides to interpreter teardown; a clean process exit
    # (no run_dtors abort) is the assertion.
    _exit_rider = _keep.pop()

print("rfc0075-sweep-regressions: ok")
